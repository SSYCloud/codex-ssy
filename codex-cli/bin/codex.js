#!/usr/bin/env node
// Unified entry point for the Codex CLI.

import { spawn } from "node:child_process";
import { existsSync, readFileSync, realpathSync, mkdirSync, chmodSync, createWriteStream, unlinkSync, rmSync } from "fs";
import { createRequire } from "node:module";
import { constants } from "node:os";
import path from "path";
import { fileURLToPath } from "url";
import https from "node:https";
import readline from "node:readline";

// __dirname equivalent in ESM
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);
const codexPackageRoot = realpathSync(path.join(__dirname, ".."));

const PLATFORM_PACKAGE_BY_TARGET = {
  "x86_64-unknown-linux-musl": "@coohu/codex-linux-x64",
  "aarch64-unknown-linux-musl": "@coohu/codex-linux-arm64",
  "x86_64-apple-darwin": "@coohu/codex-darwin-x64",
  "aarch64-apple-darwin": "@coohu/codex-darwin-arm64",
  "x86_64-pc-windows-msvc": "@coohu/codex-win32-x64",
  "aarch64-pc-windows-msvc": "@coohu/codex-win32-arm64",
};

const { platform, arch } = process;

let targetTriple = null;
switch (platform) {
  case "linux":
  case "android":
    switch (arch) {
      case "x64":
        targetTriple = "x86_64-unknown-linux-musl";
        break;
      case "arm64":
        targetTriple = "aarch64-unknown-linux-musl";
        break;
      default:
        break;
    }
    break;
  case "darwin":
    switch (arch) {
      case "x64":
        targetTriple = "x86_64-apple-darwin";
        break;
      case "arm64":
        targetTriple = "aarch64-apple-darwin";
        break;
      default:
        break;
    }
    break;
  case "win32":
    switch (arch) {
      case "x64":
        targetTriple = "x86_64-pc-windows-msvc";
        break;
      case "arm64":
        targetTriple = "aarch64-pc-windows-msvc";
        break;
      default:
        break;
    }
    break;
  default:
    break;
}

if (!targetTriple) {
  throw new Error(`Unsupported platform: ${platform} (${arch})`);
}

const platformPackage = PLATFORM_PACKAGE_BY_TARGET[targetTriple];
if (!platformPackage) {
  throw new Error(`Unsupported target triple: ${targetTriple}`);
}

// ============================================================================
// CLEANUP LOGIC FOR UNINSTALL
// ============================================================================
if (process.argv[2] === '--cleanup-binary') {
  try {
    let vendorRoot;
    try {
      const packageJsonPath = require.resolve(`${platformPackage}/package.json`);
      vendorRoot = path.join(path.dirname(packageJsonPath), "vendor");
    } catch {
      vendorRoot = path.join(__dirname, "..", "vendor");
    }
    
    if (existsSync(vendorRoot)) {
      rmSync(vendorRoot, { recursive: true, force: true });
      console.log(`\x1b[32m✅ Successfully removed downloaded binary directory: ${vendorRoot}\x1b[0m`);
    } else {
      console.log(`\x1b[33m[Codex] No local binary found to remove at ${vendorRoot}, skipping cleanup.\x1b[0m`);
    }
  } catch (err) {
    console.error(`\x1b[31m❌ Failed to clean up downloaded binary:\x1b[0m`, err.message);
    process.exit(1);
  }
  process.exit(0);
}
// ============================================================================

async function downloadBinary(destPath) {
  const isWin = process.platform === "win32";
  const assetName = isWin ? `codex-${targetTriple}.exe` : `codex-${targetTriple}`;
  const url = `https://github.com/SSYCloud/codex-ssy/releases/latest/download/${assetName}`;
  
  // Ensure the directory exists
  mkdirSync(path.dirname(destPath), { recursive: true });

  // Add some terminal coloring and an emoji for better UX
  console.log(`\x1b[36m🚀 [Codex] Local binary for ${targetTriple} not found.\x1b[0m`);
  console.log(`\x1b[33m📦 Downloading from GitHub Releases...\x1b[0m`);

  return new Promise((resolve, reject) => {
    function fetch(downloadUrl) {
      https.get(downloadUrl, (response) => {
        // Handle redirects (GitHub uses 302 for release downloads)
        if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
          return fetch(response.headers.location);
        }
        
        if (response.statusCode !== 200) {
          return reject(new Error(`Failed to download binary: HTTP ${response.statusCode} - ${response.statusMessage}`));
        }

        const totalBytes = parseInt(response.headers['content-length'], 10);
        let downloadedBytes = 0;

        const file = createWriteStream(destPath);
        response.pipe(file);

        // Listen to data chunks to update the progress bar
        response.on('data', (chunk) => {
          downloadedBytes += chunk.length;
          
          if (totalBytes) {
            const percent = Math.round((downloadedBytes / totalBytes) * 100);
            const barLength = 30; // Length of the progress bar in characters
            const completedLength = Math.round((barLength * downloadedBytes) / totalBytes);
            const bar = '█'.repeat(completedLength) + '░'.repeat(barLength - completedLength);
            
            const dlMb = (downloadedBytes / 1024 / 1024).toFixed(2);
            const totMb = (totalBytes / 1024 / 1024).toFixed(2);
            
            // Clear the current line and update the progress
            readline.cursorTo(process.stdout, 0);
            process.stdout.write(`   \x1b[32m[${bar}]\x1b[0m ${percent}% | ${dlMb} MB / ${totMb} MB`);
          } else {
            // Fallback if content-length is missing
            const dlMb = (downloadedBytes / 1024 / 1024).toFixed(2);
            readline.cursorTo(process.stdout, 0);
            process.stdout.write(`   \x1b[32mDownloading...\x1b[0m ${dlMb} MB downloaded`);
          }
        });
        
        file.on("finish", () => {
          file.close(() => {
            // Add a newline after the progress bar finishes
            console.log('\n\x1b[32m✅ Download complete. Starting Codex...\x1b[0m\n');
            if (!isWin) chmodSync(destPath, 0o755);
            resolve();
          });
        });
      }).on("error", (err) => {
        if (existsSync(destPath)) unlinkSync(destPath);
        reject(err);
      });
    }
    fetch(url);
  });
}

async function findCodexExecutable() {
  let vendorRoot;
  try {
    const packageJsonPath = require.resolve(`${platformPackage}/package.json`);
    vendorRoot = path.join(path.dirname(packageJsonPath), "vendor");
  } catch {
    vendorRoot = path.join(__dirname, "..", "vendor");
  }

  const codexExecutable = path.join(
    vendorRoot,
    targetTriple,
    "bin",
    process.platform === "win32" ? "codex.exe" : "codex",
  );
  
  if (existsSync(codexExecutable)) {
    return codexExecutable;
  }

  // Attempt to download the missing binary
  try {
    await downloadBinary(codexExecutable);
    return codexExecutable;
  } catch (downloadError) {
    console.error("\n\x1b[31m❌ Failed to download the executable automatically:\x1b[0m", downloadError.message);
    
    // Fallback to the original instruction if download fails
    const packageManager = detectPackageManager();
    const updateCommand =
      packageManager === "bun"
        ? "bun install -g @coohu/codex@latest"
        : packageManager === "pnpm"
          ? "pnpm add -g @coohu/codex@latest"
          : packageManager === "vite-plus"
            ? "vp install -g @coohu/codex@latest"
            : "npm install -g @coohu/codex@latest";
    throw new Error(
      `Missing optional dependency ${platformPackage} and download failed. Reinstall Codex: ${updateCommand}`,
    );
  }
}

const binaryPath = await findCodexExecutable();

function isPnpmOwnedCodexInstall(nodeModulesDir) {
  if (!existsSync(path.join(nodeModulesDir, ".modules.yaml"))) {
    return false;
  }

  try {
    return (
      realpathSync(path.join(nodeModulesDir, "@coohu", "codex")) ===
      codexPackageRoot
    );
  } catch {
    return false;
  }
}

function isVitePlusOwnedCodexInstall(packagesDir) {
  if (path.basename(packagesDir) !== "packages") {
    return false;
  }

  try {
    const metadata = JSON.parse(
      readFileSync(path.join(packagesDir, "@coohu", "codex.json"), "utf8"),
    );
    if (metadata.name !== "@coohu/codex") {
      return false;
    }

    const installId = metadata.installId || "";
    const installDir = installId.startsWith("#")
      ? path.join(packagesDir, `@coohu/codex${installId}`)
      : path.join(packagesDir, "@coohu/codex", installId);
    for (const nodeModulesDir of [
      path.join(installDir, "lib", "node_modules"),
      path.join(installDir, "node_modules"),
    ]) {
      const packageRoot = path.join(nodeModulesDir, "@coohu", "codex");
      if (
        existsSync(packageRoot) &&
        realpathSync(packageRoot) === codexPackageRoot
      ) {
        return true;
      }
    }
  } catch {
  }
  return false;
}

function detectPackageManager() {
  const entrypointDir = path.dirname(path.resolve(process.argv[1]));
  for (const startDir of new Set([codexPackageRoot, entrypointDir])) {
    const filesystemRoot = path.parse(startDir).root;
    for (
      let currentDir = startDir;
      currentDir !== filesystemRoot;
      currentDir = path.dirname(currentDir)
    ) {
      if (isVitePlusOwnedCodexInstall(currentDir)) {
        return "vite-plus";
      }
      if (isPnpmOwnedCodexInstall(path.join(currentDir, "node_modules"))) {
        return "pnpm";
      }
    }

    if (isPnpmOwnedCodexInstall(path.join(filesystemRoot, "node_modules"))) {
      return "pnpm";
    }
  }

  const userAgent = process.env.npm_config_user_agent || "";
  if (/\bbun\//.test(userAgent)) {
    return "bun";
  }

  const execPath = process.env.npm_execpath || "";
  if (execPath.includes("bun")) {
    return "bun";
  }

  if (
    __dirname.includes(".bun/install/global") ||
    __dirname.includes(".bun\\install\\global")
  ) {
    return "bun";
  }

  return userAgent ? "npm" : null;
}

const packageManager = detectPackageManager();
const packageManagerEnvVar =
  packageManager === "bun"
    ? "CODEX_MANAGED_BY_BUN"
    : packageManager === "pnpm"
      ? "CODEX_MANAGED_BY_PNPM"
      : packageManager === "vite-plus"
        ? "CODEX_MANAGED_BY_VITE_PLUS"
        : "CODEX_MANAGED_BY_NPM";
const env = {
  ...process.env,
  CODEX_MANAGED_PACKAGE_ROOT: codexPackageRoot,
};
delete env.CODEX_MANAGED_BY_NPM;
delete env.CODEX_MANAGED_BY_BUN;
delete env.CODEX_MANAGED_BY_PNPM;
delete env.CODEX_MANAGED_BY_VITE_PLUS;
env[packageManagerEnvVar] = "1";

// Replace Node so the native binary receives signals and exits directly.
// Node exposes execve on Windows too, but calling it there is unsupported.
if (process.platform !== "win32" && typeof process.execve === "function") {
  // Node ignores SIGXFSZ, and ignored signals survive execve. Installing a
  // handler lets execve restore the default disposition in the native binary.
  process.on("SIGXFSZ", () => {});
  process.execve(binaryPath, [binaryPath, ...process.argv.slice(2)], env);
}

// Older Node versions and Windows need a child process. Spawn asynchronously
// so Node can forward signals while the native binary is executing.
const child = spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
  env,
});

child.on("error", (err) => {
  console.error(err);
  process.exit(1);
});

const forwardSignal = (signal) => {
  if (child.killed) {
    return;
  }
  try {
    child.kill(signal);
  } catch {
  }
};

["SIGINT", "SIGTERM", "SIGHUP"].forEach((sig) => {
  process.on(sig, () => forwardSignal(sig));
});

const childResult = await new Promise((resolve) => {
  child.on("exit", (code, signal) => {
    if (signal) {
      resolve({ type: "signal", signal });
    } else {
      resolve({ type: "code", exitCode: code ?? 1 });
    }
  });
});

if (childResult.type === "signal") {
  // Preserve a failure exit code even if Node ignores the re-emitted signal.
  process.exitCode = 128 + constants.signals[childResult.signal];
  process.removeAllListeners(childResult.signal);
  process.kill(process.pid, childResult.signal);
} else {
  process.exit(childResult.exitCode);
}
