# npm releases

Use the staging helper in the repo root to generate npm tarballs for a release. For
example, to stage the CLI, responses proxy, and SDK packages for version `0.6.0`:

```bash
./scripts/stage_npm_packages.py \
  --release-version 0.6.0 \
  --package codex \
  --package codex-responses-api-proxy \
  --package codex-sdk

  python3 build_npm_package.py \
  --package codex \
  --version 0.1.0 \
  --vendor-src /Users/zou/Downloads/bin \
  --pack-output ./dist/codex-0.1.0.tgz
```

This downloads the required native package archive artifacts, hydrates `vendor/` for
each package, and writes tarballs to `dist/npm/`.

When `--package codex` is provided, the staging helper builds the lightweight
`@coohu/codex` meta package plus all platform-native `@coohu/codex` variants
that are later published under platform-specific dist-tags.

The npm variants omit the standalone `codex-exec-server` executable and its
`execServer` manifest entry to keep uploads within npm's package size limit.
They retain `codex exec-server` through the CLI, the code-mode host, and the
platform sandbox helpers. Canonical release archives retain the standalone
executor; npm staging changes only the package copy.

Direct `build_npm_package.py` invocations are still useful for package-specific
debugging, but native packages expect `--vendor-src` to point at a prehydrated
`vendor/` tree. Release packaging should use `scripts/stage_npm_packages.py`


`build_npm_package.py` 是一个 Python 脚本，主要用于为 `@coohu/codex` 及其相关的跨平台 Native 二进制文件、SDK 以及 Proxy 代理组件**准备（Stage）和打 npm 安装包（`.tgz`）**。

---

### **核心使用方式与基本语法**

使用 `python3 build_npm_package.py` 运行该脚本，必须通过参数指定要打包的组件以及版本号。

```bash
python3 build_npm_package.py --package <组件名称> --version <版本号> [其他选项]

```

---

### **命令行参数详解**

* **`--package <名称>`** *(可选)*
* **作用**：指定要构建/阶段化的组件。


* **可选值**：
* `codex` *(默认)*：构建主 CLI 入口包。


* `codex-linux-x64` / `codex-linux-arm64`：构建对应 Linux 平台的二进制包。


* `codex-darwin-x64` / `codex-darwin-arm64`：构建对应 macOS 平台的二进制包。


* `codex-win32-x64` / `codex-win32-arm64`：构建对应 Windows 平台的二进制包。


* `codex-responses-api-proxy`：构建 API 响应代理服务组件。


* `codex-sdk`：构建 TypeScript SDK 组件。






* **`--version <版本号>`** / **`--release-version <版本号>`** *(必填)*
* **作用**：写入打包后 `package.json` 中的版本号。如果是发布模式，推荐使用 `--release-version`，它会在控制台打印对应的安装与验证命令。




* **`--vendor-src <路径>`** *(平台二进制包必填)*
* **作用**：指定包含预编译好的原生二进制文件（Native Binaries）的目录。构建原生平台包（如 `codex-linux-x64` 等）时必须提供此参数。




* **`--staging-dir <路径>`** *(可选)*
* **作用**：指定用于暂存打包文件的目录。**必须是空目录**；如果不填，脚本会自动创建一个临时目录。




* **`--pack-output <路径>`** *(可选)*
* **作用**：指定生成 `.tgz` 安装包（`npm pack`）的输出路径。如果配置了该参数，脚本会在暂存完成后自动执行 `npm pack` 并将压缩包移动到指定位置。





---

### **常见使用场景示例**

#### **1. 构建主 CLI 入口包 (`codex`)**

阶段化主包，并直接导出 `.tgz` 文件：

```bash
python3 build_npm_package.py \
  --package codex \
  --version 0.1.0 \
  --pack-output ./dist/codex-0.1.0.tgz

```

#### **2. 为特定的平台（如 macOS ARM64）打包二进制原生组件**

打包特定平台架构的 Native 依赖，需要配合 `--vendor-src` 指向预编译好的二进制文件目录：

```bash
python3 build_npm_package.py \
  --package codex-darwin-arm64 \
  --version 0.1.0 \
  --vendor-src ./prebuilt-binaries \
  --pack-output ./dist/codex-darwin-arm64-0.1.0.tgz

```

#### **3. 构建并阶段化 TypeScript SDK**

自动调用 `pnpm install` 和 `pnpm run build` 编译 SDK，并将产物暂存到指定目录：

```bash
python3 build_npm_package.py \
  --package codex-sdk \
  --release-version 0.1.0 \
  --staging-dir ./staging/sdk

```

---

### **工作流程说明**

1. **环境与路径校验**：解析参数，检查暂存目录是否为空。


2. **源码与配置文件整理（Staging）**：
* 如果是主包 `codex`，复制 `bin/codex.js` 并生成带有跨平台可选依赖（`optionalDependencies`）的 `package.json`。


* 如果是 SDK，自动触发 `pnpm` 构建并将 `dist` 产物拷贝至暂存区。




3. **原生二进制文件复制**：校验并从 `--vendor-src` 复制对应平台的二进制文件至暂存区的 `vendor` 目录。


4. **生成 NPM 压缩包**：如果指定了 `--pack-output`，脚本会使用隔离的环境变量（如独立的 `NPM_CONFIG_CACHE`）调用 `npm pack` 生成 `.tgz` 包。



npm publish ./dist/codex-0.153.8.tgz --access public