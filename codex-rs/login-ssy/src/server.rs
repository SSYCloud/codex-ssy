use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::io::{self};
use std::net::SocketAddr;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use chrono::Utc;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::auth::AuthDotJson;
use codex_core::auth::save_auth;
use codex_core::token_data::TokenData;
use codex_core::token_data::parse_id_token;
use tiny_http::Header;
use tiny_http::Request;
use tiny_http::Response;
use tiny_http::Server;
use tiny_http::StatusCode;

const DEFAULT_ISSUER: &str = "https://router.shengsuanyun.com/auth";
const DEFAULT_PORT: u16 = 1455;

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub codex_home: PathBuf,
    pub client_id: String,
    pub issuer: String,
    pub port: u16,
    pub open_browser: bool,
    pub force_state: Option<String>,
    pub forced_chatgpt_workspace_id: Option<String>,
    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
}

impl ServerOptions {
    pub fn new(
        codex_home: PathBuf,
        client_id: String,
        forced_chatgpt_workspace_id: Option<String>,
        cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            codex_home,
            client_id,
            issuer: DEFAULT_ISSUER.to_string(),
            port: DEFAULT_PORT,
            open_browser: true,
            force_state: None,
            forced_chatgpt_workspace_id,
            cli_auth_credentials_store_mode,
        }
    }
}

pub struct LoginServer {
    pub auth_url: String,
    pub actual_port: u16,
    server_handle: tokio::task::JoinHandle<io::Result<()>>,
    shutdown_handle: ShutdownHandle,
}

impl LoginServer {
    pub async fn block_until_done(self) -> io::Result<()> {
        self.server_handle
            .await
            .map_err(|err| io::Error::other(format!("login server thread panicked: {err:?}")))?
    }

    pub fn cancel(&self) {
        self.shutdown_handle.shutdown();
    }

    pub fn cancel_handle(&self) -> ShutdownHandle {
        self.shutdown_handle.clone()
    }
}

#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    shutdown_notify: Arc<tokio::sync::Notify>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        self.shutdown_notify.notify_waiters();
    }
}

pub fn run_login_server(opts: ServerOptions) -> io::Result<LoginServer> {
    let server = bind_server(opts.port)?;
    let actual_port = match server.server_addr().to_ip() {
        Some(addr) => addr.port(),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Unable to determine the server port",
            ));
        }
    };
    let server = Arc::new(server);

    let redirect_uri = format!("http://localhost:{actual_port}/auth/callback");
    let auth_url = build_authorize_url(
        &opts.issuer,
        &opts.client_id,
        &redirect_uri
    );

    if opts.open_browser {
        let _ = webbrowser::open(&auth_url);
    }

    // Map blocking reads from server.recv() to an async channel.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Request>(16);
    let _server_handle = {
        let server = server.clone();
        thread::spawn(move || -> io::Result<()> {
            while let Ok(request) = server.recv() {
                tx.blocking_send(request).map_err(|e| {
                    eprintln!("Failed to send request to channel: {e}");
                    io::Error::other("Failed to send request to channel")
                })?;
            }
            Ok(())
        })
    };

    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let server_handle = {
        let shutdown_notify = shutdown_notify.clone();
        let server = server;
        tokio::spawn(async move {
            let result = loop {
                tokio::select! {
                    _ = shutdown_notify.notified() => {
                        break Err(io::Error::other("Login was not completed"));
                    }
                    maybe_req = rx.recv() => {
                        let Some(req) = maybe_req else {
                            break Err(io::Error::other("Login was not completed"));
                        };

                        let url_raw = req.url().to_string();
                        let response =
                            process_request(&url_raw, &opts, &redirect_uri).await;

                        let exit_result = match response {
                            HandledRequest::Response(response) => {
                                let _ = tokio::task::spawn_blocking(move || req.respond(response)).await;
                                None
                            }
                            HandledRequest::ResponseAndExit {
                                headers,
                                body,
                                result,
                            } => {
                                let _ = tokio::task::spawn_blocking(move || {
                                    send_response_with_disconnect(req, headers, body)
                                })
                                .await;
                                Some(result)
                            }
                            HandledRequest::RedirectWithHeader(header) => {
                                let redirect = Response::empty(302).with_header(header);
                                let _ = tokio::task::spawn_blocking(move || req.respond(redirect)).await;
                                None
                            }
                        };

                        if let Some(result) = exit_result {
                            break result;
                        }
                    }
                }
            };

            // Ensure that the server is unblocked so the thread dedicated to
            // running `server.recv()` in a loop exits cleanly.
            server.unblock();
            result
        })
    };

    Ok(LoginServer {
        auth_url,
        actual_port,
        server_handle,
        shutdown_handle: ShutdownHandle { shutdown_notify },
    })
}

enum HandledRequest {
    Response(Response<Cursor<Vec<u8>>>),
    #[allow(dead_code)]
    RedirectWithHeader(Header),
    ResponseAndExit {
        headers: Vec<Header>,
        body: Vec<u8>,
        result: io::Result<()>,
    },
}

async fn process_request(
    url_raw: &str,
    opts: &ServerOptions,
    redirect_uri: &str,
) -> HandledRequest {
    let parsed_url = match url::Url::parse(&format!("http://localhost{url_raw}")) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("URL parse error: {e}");
            return HandledRequest::Response(
                Response::from_string("Bad Request").with_status_code(400),
            );
        }
    };
    let path = parsed_url.path().to_string();

    match path.as_str() {
        "/auth/callback" => {
            let params: std::collections::HashMap<String, String> =
                parsed_url.query_pairs().into_owned().collect();
            let code = match params.get("code") {
                Some(c) if !c.is_empty() => c.clone(),
                _ => {
                    return HandledRequest::Response(
                        Response::from_string("Missing authorization code").with_status_code(400),
                    );
                }
            };

            match exchange_code_for_tokens(&redirect_uri, &code).await
            {
                Ok(tokens) => {
                    // Obtain API key via token-exchange and persist
                    let api_key = tokens.access_token.clone();
                    if let Err(err) = persist_tokens_async(
                        &opts.codex_home,
                        Some(api_key.clone()),
                        tokens.id_token.clone(),
                        tokens.access_token.clone(),
                        tokens.refresh_token.clone(),
                        opts.cli_auth_credentials_store_mode,
                    ).await{
                        eprintln!("Persist error: {err}");
                        return HandledRequest::Response(
                            Response::from_string(format!("Unable to persist auth file: {err}"))
                                .with_status_code(500),
                        );
                    }
                    HandledRequest::ResponseAndExit {
                        headers: Vec::new(),
                        body: include_str!("assets/success.html").as_bytes().to_vec(),
                        result: Ok(()),
                    }
                }
                Err(err) => {
                    eprintln!("Token exchange error: {err}");
                    HandledRequest::Response(
                        Response::from_string(format!("Token exchange failed: {err}"))
                            .with_status_code(500),
                    )
                }
            }
        }
        "/success" => {
            let body = include_str!("assets/success.html");
            HandledRequest::ResponseAndExit {
                headers: match Header::from_bytes(
                    &b"Content-Type"[..],
                    &b"text/html; charset=utf-8"[..],
                ) {
                    Ok(header) => vec![header],
                    Err(_) => Vec::new(),
                },
                body: body.as_bytes().to_vec(),
                result: Ok(()),
            }
        }
        "/cancel" => HandledRequest::ResponseAndExit {
            headers: Vec::new(),
            body: b"Login cancelled".to_vec(),
            result: Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Login cancelled",
            )),
        },
        _ => HandledRequest::Response(Response::from_string("Not Found").with_status_code(404)),
    }
}

/// tiny_http filters `Connection` headers out of `Response` objects, so using
/// `req.respond` never informs the client (or the library) that a keep-alive
/// socket should be closed. That leaves the per-connection worker parked in a
/// loop waiting for more requests, which in turn causes the next login attempt
/// to hang on the old connection. This helper bypasses tiny_http’s response
/// machinery: it extracts the raw writer, prints the HTTP response manually,
/// and always appends `Connection: close`, ensuring the socket is closed from
/// the server side. Ideally, tiny_http would provide an API to control
/// server-side connection persistence, but it does not.
fn send_response_with_disconnect(
    req: Request,
    mut headers: Vec<Header>,
    body: Vec<u8>,
) -> io::Result<()> {
    let status = StatusCode(200);
    let mut writer = req.into_writer();
    let reason = status.default_reason_phrase();
    write!(writer, "HTTP/1.1 {} {}\r\n", status.0, reason)?;
    headers.retain(|h| !h.field.equiv("Connection"));
    if let Ok(close_header) = Header::from_bytes(&b"Connection"[..], &b"close"[..]) {
        headers.push(close_header);
    }

    let content_length_value = format!("{}", body.len());
    if let Ok(content_length_header) =
        Header::from_bytes(&b"Content-Length"[..], content_length_value.as_bytes())
    {
        headers.push(content_length_header);
    }

    for header in headers {
        write!(
            writer,
            "{}: {}\r\n",
            header.field.as_str(),
            header.value.as_str()
        )?;
    }

    writer.write_all(b"\r\n")?;
    writer.write_all(&body)?;
    writer.flush()
}

fn build_authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str
) -> String {
    let query = vec![
        ("from".to_string(), client_id.to_string()),
        ("callback_url".to_string(), redirect_uri.to_string()),
    ];
    let qs = query
        .into_iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(&v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{issuer}?{qs}")
}

fn send_cancel_request(port: u16) -> io::Result<()> {
    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    stream.write_all(b"GET /cancel HTTP/1.1\r\n")?;
    stream.write_all(format!("Host: 127.0.0.1:{port}\r\n").as_bytes())?;
    stream.write_all(b"Connection: close\r\n\r\n")?;

    let mut buf = [0u8; 64];
    let _ = stream.read(&mut buf);
    Ok(())
}

fn bind_server(port: u16) -> io::Result<Server> {
    let bind_address = format!("127.0.0.1:{port}");
    let mut cancel_attempted = false;
    let mut attempts = 0;
    const MAX_ATTEMPTS: u32 = 10;
    const RETRY_DELAY: Duration = Duration::from_millis(200);

    loop {
        match Server::http(&bind_address) {
            Ok(server) => return Ok(server),
            Err(err) => {
                attempts += 1;
                let is_addr_in_use = err
                    .downcast_ref::<io::Error>()
                    .map(|io_err| io_err.kind() == io::ErrorKind::AddrInUse)
                    .unwrap_or(false);

                // If the address is in use, there is probably another instance of the login server
                // running. Attempt to cancel it and retry.
                if is_addr_in_use {
                    if !cancel_attempted {
                        cancel_attempted = true;
                        if let Err(cancel_err) = send_cancel_request(port) {
                            eprintln!("Failed to cancel previous login server: {cancel_err}");
                        }
                    }

                    thread::sleep(RETRY_DELAY);

                    if attempts >= MAX_ATTEMPTS {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            format!("Port {bind_address} is already in use"),
                        ));
                    }

                    continue;
                }

                return Err(io::Error::other(err));
            }
        }
    }
}

pub(crate) struct ExchangedTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
}

pub(crate) async fn exchange_code_for_tokens(
    redirect_uri: &str,
    code: &str,
) -> io::Result<ExchangedTokens> {
    let client = reqwest::Client::new();
    let resp = client.post("https://api.shengsuanyun.com/auth/keys")
        .header("Content-Type", "application/json")
        .body({
            serde_json::json!({"code": code,"redirect_uri": redirect_uri})
            .to_string()
        })
        .send()
        .await
        .map_err(io::Error::other)?;

    if !resp.status().is_success() {
        return Err(io::Error::other(format!(
            "token endpoint returned status {}",
            resp.status()
        )));
    }
     #[derive(serde::Deserialize)]
    struct Data {
        api_key: String,
        jwt_token:String,
    }
    #[derive(serde::Deserialize)]
    struct Res {
        data: Data,
    }
    let tokens: Res = resp.json().await.map_err(io::Error::other)?;
    Ok(ExchangedTokens {
        id_token: tokens.data.jwt_token,
        access_token: tokens.data.api_key.clone(),
        refresh_token: tokens.data.api_key,
    })
}

pub(crate) async fn persist_tokens_async(
    codex_home: &Path,
    api_key: Option<String>,
    id_token: String,
    access_token: String,
    refresh_token: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    // Reuse existing synchronous logic but run it off the async runtime.
    let codex_home = codex_home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let tokens = TokenData {
            id_token: parse_id_token(&id_token).map_err(io::Error::other)?,
            access_token,
            refresh_token,
            account_id: None,
        };
        let auth = AuthDotJson {
            openai_api_key: api_key,
            tokens: Some(tokens),
            last_refresh: Some(Utc::now()),
        };
        save_auth(&codex_home, &auth, auth_credentials_store_mode)
    })
    .await
    .map_err(|e| io::Error::other(format!("persist task failed: {e}")))?
}
#[allow(dead_code)]
// Respond to the oauth server with an error so the code becomes unusable by anybody else.
fn login_error_response(message: &str) -> HandledRequest {
    let mut headers = Vec::new();
    if let Ok(header) = Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..])
    {
        headers.push(header);
    }
    HandledRequest::ResponseAndExit {
        headers,
        body: message.as_bytes().to_vec(),
        result: Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            message.to_string(),
        )),
    }
}
