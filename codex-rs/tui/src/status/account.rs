#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusAccountDisplay {
    ShengSuanYun {
        name: Option<String>,
        plan: Option<String>,
    },
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    ApiKey,
}
