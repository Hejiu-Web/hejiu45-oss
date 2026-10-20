以下是完整的实施方案，围绕你新增的两个核心诉求展开：**Linux/Windows 命令级快速安装**，以及**Vue3 前端（JavaScript，不使用 TypeScript）嵌入 Rust 后端形成单二进制**。

整体策略是：**前端构建产物在编译期嵌入 Rust 二进制，最终每个平台只产出一个可执行文件**。Linux 侧使用静态链接的 musl 二进制，Windows 侧使用原生 `.exe`，用户通过一条命令即可完成下载、安装和启动。

---

## 🏗️ 总体架构：单二进制 + 嵌入前端

```text
┌─────────────────────────────────────────────┐
│              Axum Server（单二进制）          │
│  ┌─────────────────────────────────────┐    │
│  │  /api/*  →  S3 核心逻辑 / 管理 API    │    │
│  │  /*      → 嵌入的 Vue3 前端（SPA）     │    │
│  └─────────────────────────────────────┘    │
├─────────────────────────────────────────────┤
│  Auth 层（SigV4）                            │
├─────────────────────────────────────────────┤
│  MetaEngine（redb）                          │
├─────────────────────────────────────────────┤
│  DataEngine（本地文件系统，流式 I/O）          │
└─────────────────────────────────────────────┘
         ▲
         │  rust-embed 在编译期将 frontend/dist/ 写入二进制
         │
┌────────┴────────┐
│  Vue3 + Vite     │  npm run build → dist/
│  (JavaScript)    │
└──────────────────┘
```

开发阶段，Axum 将前端请求代理到 Vite Dev Server，保留 HMR；生产构建时，`rust-embed` 在编译期把 `frontend/dist/` 的全部文件嵌入二进制，**运行时不需要任何静态文件目录**。

---

## 📦 一、跨平台编译：产出两个“裸二进制”

### 🐧 Linux：静态 musl 二进制

为了避免依赖目标服务器的 glibc 版本，Linux 产物使用 `x86_64-unknown-linux-musl` 目标进行静态链接。静态二进制可以在“广泛的系统范围内”运行，不依赖系统 libc。

**方式 A：在 Linux 或 macOS 上直接编译**

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

产物位于 `target/x86_64-unknown-linux-musl/release/<你的二进制名>`。

**方式 B：在 Windows 上交叉编译 Linux 产物（推荐使用 cargo-zigbuild）**

如果你主要在 Windows 上开发，`cargo-zigbuild` 配合 Zig 链接器可以方便地产出 Linux musl 静态二进制：

```powershell
cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-musl
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

### 🪟 Windows：原生 `.exe`

在 Windows 上直接执行标准 release 构建即可，Rust 默认会产出静态链接的 `.exe`，运行时不需要额外依赖：

```powershell
cargo build --release
```

产物为 `target\release\<你的二进制名>.exe`。

> **关于前端构建的时机**：`rust-embed` 在**编译期**读取 `frontend/dist/`。因此，在运行 `cargo build --release` 或 `cargo zigbuild` **之前**，必须先在前端目录执行 `npm run build`，确保 `dist/` 是最新的。可以将这一步封装到 `build.rs` 或 CI 脚本中。

---

## 🎨 二、Vue3 前端：JavaScript + Vite，构建后嵌入

### ⚙️ 前端初始化（纯 JavaScript）

```bash
npm create vite@latest frontend -- --template vue
cd frontend
npm install
npm install vue-router
```

不要选择 `vue-ts`，使用 `--template vue` 即可得到纯 JavaScript 的 Vue3 项目。

### ⚙️ Vite 关键配置（`frontend/vite.config.js`）

```javascript
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:3000', // 开发时 API 代理到 Axum
    },
  },
})
```

开发时，Vite 运行在 5173 端口，`/api` 请求代理到 Axum 的 3000 端口，前后端可以独立热更新。

### ⚙️ Vue Router 使用 Hash 模式（简化 SPA fallback）

为了减少 Rust 侧 SPA fallback 的复杂度，前端路由建议使用 **`createWebHashHistory`**：

```javascript
import { createRouter, createWebHashHistory } from 'vue-router'

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', component: () => import('./views/Home.vue') },
    { path: '/buckets', component: () => import('./views/Buckets.vue') },
  ],
})
```

Hash 模式下，所有前端路由都在 `#` 之后，浏览器请求的路径始终是 `/`，Rust 侧只需要在根路径返回 `index.html` 即可，不需要为每个前端路由做 fallback 判断。

### ⚙️ 前端 API 调用

管理端的 API 请求统一走 `/api` 前缀：

```javascript
const res = await fetch('/api/buckets')
```

S3 客户端仍然走标准的 S3 端点（可以是同一个 Axum 服务的根路径或独立端口），与前端管理 API 在路径上分离。

### ⚙️ 构建产物

```bash
cd frontend
npm run build
```

生成 `frontend/dist/`，包含 `index.html`、`assets/*.js`、`assets/*.css` 等文件。这些文件将被 Rust 二进制“吞入”。

---

## 🦀 三、Rust 后端：嵌入前端 + SPA 服务

### 📚 依赖（`Cargo.toml` 关键部分）

```toml
[dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full"] }
rust-embed = "8"
mime_guess = "2"
tower-http = { version = "0.6", features = ["fs", "trace"] }
redb = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### 📚 嵌入前端资源

```rust
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;
```

`rust-embed` 在**编译期**读取 `frontend/dist/`，将每个文件嵌入二进制，运行时无需文件系统访问。

### 📚 Axum 路由：API 优先，SPA 兜底

```rust
use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap())
                .body(Body::from(content.data))
                .unwrap()
        }
        None => {
            // SPA fallback：非 API 路径一律返回 index.html
            match Assets::get("index.html") {
                Some(index) => Response::builder()
                    .header(header::CONTENT_TYPE, "text/html")
                    .body(Body::from(index.data))
                    .unwrap(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

pub fn build_router() -> Router {
    Router::new()
        // 管理 API
        .route("/api/health", get(|| async { "ok" }))
        // S3 子集路由放在 /api/ 之前或独立路径，确保不被 SPA fallback 吞掉
        .fallback(static_handler)
}
```

关键点：**API 路由必须注册在 `fallback` 之前**，否则 `/api/*` 请求会被 SPA fallback 误吞。如果 S3 端点也走同一 Router，需要将 S3 路径（如 `/*bucket/*key`）显式 `route` 到 S3 handler，而不是交给 `fallback`。

### 💡 更省事的替代方案：`axum-vite` 或 `vite-rs`

如果你不想手写 SPA fallback 和缓存头逻辑，可以直接使用 `axum-vite` crate。它在开发时代理到 Vite Dev Server（保留 HMR），生产时通过 `include_dir` 将前端嵌入二进制，并自动处理 ETag、`Cache-Control` 和 304 响应。`vite-rs` 是另一个同类选择，提供与 Axum 集成的 Tower service。

---

## 🚀 四、Linux 一键安装：静态二进制 + systemd

用户拿到的是单个静态二进制文件。安装脚本完成：下载 → 赋权 → 创建系统用户 → 创建数据目录 → 写入 systemd 单元 → 启动服务。

### 💻 `install-linux.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail

BINARY_NAME="myoss"
INSTALL_DIR="/usr/local/bin"
DATA_DIR="/var/lib/myoss"
SERVICE_FILE="/etc/systemd/system/myoss.service"
DOWNLOAD_URL="${MYOSS_DOWNLOAD_URL:-https://your-domain.com/releases/latest/myoss-linux-amd64}"

# 1. 下载二进制
curl -fsSL "$DOWNLOAD_URL" -o "/tmp/${BINARY_NAME}"
chmod +x "/tmp/${BINARY_NAME}"
mv "/tmp/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"

# 2. 创建系统用户和数据目录
id -u myoss &>/dev/null || useradd --system --no-create-home --shell /usr/sbin/nologin myoss
mkdir -p "$DATA_DIR"
chown -R myoss:myoss "$DATA_DIR"

# 3. 写入 systemd 单元
cat > "$SERVICE_FILE" <<'EOF'
[Unit]
Description=MyOSS Object Storage
After=network.target

[Service]
Type=simple
User=myoss
Group=myoss
ExecStart=/usr/local/bin/myoss --data-dir /var/lib/myoss --listen 0.0.0.0:3000
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

# 4. 启动
systemctl daemon-reload
systemctl enable --now myoss

echo "安装完成。服务状态："
systemctl status myoss --no-pager
```

用户执行一条命令即可完成：

```bash
curl -fsSL https://your-domain.com/install-linux.sh | bash
```

这个模式与 MinIO 的二进制 + systemd 部署方式完全一致：下载二进制、赋权、创建用户、配置 systemd、启动服务。

---

## 🚀 五、Windows 一键安装：`.exe` + NSSM 注册服务

Windows 侧的挑战在于：普通的 `.exe` 不是原生 Windows 服务，`sc create` 无法直接将其注册为服务。**最稳妥的方案是使用 NSSM（Non-Sucking Service Manager）作为包装器**，它可以将任意控制台程序注册为 Windows 服务，并支持日志轮转和崩溃恢复。

### 💻 `install-windows.ps1`

```powershell
# install-windows.ps1
$ErrorActionPreference = "Stop"

$BinaryName = "myoss.exe"
$InstallDir = "$env:ProgramFiles\MyOSS"
$DataDir    = "$env:ProgramData\MyOSS"
$DownloadUrl = if ($env:MYOSS_DOWNLOAD_URL) { $env:MYOSS_DOWNLOAD_URL } else { "https://your-domain.com/releases/latest/myoss-windows-amd64.exe" }

# 1. 下载二进制
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Invoke-WebRequest -Uri $DownloadUrl -OutFile (Join-Path $InstallDir $BinaryName)

# 2. 创建数据目录
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null

# 3. 获取 NSSM（如果尚未安装）
$Nssm = "$env:ProgramFiles\MyOSS\nssm.exe"
if (-not (Test-Path $Nssm)) {
    $NssmZip = "$env:TEMP\nssm.zip"
    Invoke-WebRequest -Uri "https://nssm.cc/release/nssm-2.24.zip" -OutFile $NssmZip
    Expand-Archive -Path $NssmZip -DestinationPath "$env:TEMP\nssm" -Force
    Copy-Item "$env:TEMP\nssm\nssm-2.24\win64\nssm.exe" $Nssm
}

# 4. 注册并启动服务
& $Nssm install MyOSS (Join-Path $InstallDir $BinaryName) "--data-dir `"$DataDir`" --listen 0.0.0.0:3000"
& $Nssm set MyOSS AppDirectory $InstallDir
& $Nssm set MyOSS Start SERVICE_AUTO_START
& $Nssm start MyOSS

Write-Host "安装完成。服务状态："
Get-Service MyOSS
```

用户以**管理员身份**打开 PowerShell，执行一条命令：

```powershell
powershell -ExecutionPolicy Bypass -Command "irm https://your-domain.com/install-windows.ps1 | iex"
```

或下载后执行：

```powershell
.\install-windows.ps1
```

> **备选方案**：如果不想引入 NSSM 的外部依赖，可以使用 **WinSW**，它通过一个 XML 配置文件将普通 `.exe` 包装为 Windows 服务。但 NSSM 的优势是“一行命令注册，自动处理控制台程序的服务化细节”，对一键安装脚本更友好。

---

## 🗂️ 六、项目结构与构建流程

```text
myoss/
├── frontend/                 # Vue3 + Vite（JavaScript）
│   ├── src/
│   ├── vite.config.js
│   └── package.json
├── src/
│   ├── main.rs
│   ├── api/                  # Axum 路由
│   ├── auth/                 # SigV4 验证
│   ├── meta/                 # redb 元数据
│   └── data/                 # 文件系统数据层
├── Cargo.toml
├── install-linux.sh
└── install-windows.ps1
```

**构建顺序**（关键，顺序不能颠倒）：

```bash
# 1. 构建前端
cd frontend && npm run build && cd ..

# 2. 构建 Rust 二进制（前端 dist/ 会被 rust-embed 嵌入）
# Linux（从任何平台）
cargo zigbuild --release --target x86_64-unknown-linux-musl

# Windows（在 Windows 上）
cargo build --release
```

最终交付物：

| 平台    | 产物                               | 用户安装方式                                               |
| :------ | :--------------------------------- | :--------------------------------------------------------- |
| Linux   | 单个静态二进制（无任何动态库依赖） | `curl -fsSL .../install-linux.sh \| bash`                  |
| Windows | 单个 `.exe`                        | `powershell -Command "irm .../install-windows.ps1 \| iex"` |

---

## ⚠️ 需要留意的几个点

**Windows 上的原子 rename**：`std::fs::rename` 在 Windows 上当目标文件已存在时会失败，而 Linux 会静默覆盖。如果你的 PUT 流程依赖“临时文件 + rename”来保证原子性，需要在 Windows 分支中先删除目标文件（或使用 `ReplaceFile` API），否则 Windows 上的覆盖写会出错。

**musl 静态二进制的性能**：musl 的 malloc 实现在某些高并发分配场景下可能略逊于 glibc，对于你的“轻量对象存储”场景通常可以忽略，但如果后续发现分配器成为瓶颈，可以切换到 `mimalloc` 或 `jemalloc` 作为全局分配器，它们与 musl 静态链接兼容良好。

**前端嵌入后的二进制体积**：Vue3 + 路由 + 少量组件构建后的 `dist/` 通常在几百 KB 到 1–2 MB 量级，嵌入后对二进制总体积的影响很小，仍然可以保持在数十 MB 的“轻量”范围内。

**S3 端点与管理 API 的路径隔离**：如果 S3 客户端和管理前端共用同一个 Axum 端口，务必在路由层面把 S3 路径（通常是 `/*bucket/*key` 风格）显式注册到 S3 handler，避免被 SPA fallback 截获。更简单的做法是**将管理 API 和 S3 端点放在不同端口**（例如 S3 在 3000，管理前端/API 在 3001），路由完全隔离，SPA fallback 不会与 S3 路径产生冲突。

核心思路是：**把“登录”和“S3 数据访问”拆成两条完全独立的安全路径**。管理端的登录做得极简（单用户或极少用户，Cookie Session），但 S3 数据面必须走 **SigV4 签名验证**，这是保证“OSS 数据安全”不可妥协的底线。两条路径互不干扰，攻击面反而更小。

### 🔐 两条安全路径的明确分工

|                  | **管理端登录**                                      | **S3 数据面访问**                                      |
| :--------------- | :-------------------------------------------------- | :----------------------------------------------------- |
| **保护对象**     | Web 控制台（Bucket 管理、用户管理）                 | 实际的 Object 读写（PUT/GET/DELETE）                   |
| **凭证形态**     | 用户名 + 密码 → 服务端 Session（不透明 Session ID） | Access Key ID + Secret Access Key                      |
| **传输载体**     | `HttpOnly; Secure; SameSite=Strict` Cookie          | `Authorization: AWS4-HMAC-SHA256 ...` 请求头           |
| **核心安全机制** | Argon2id 密码哈希 + 服务端会话即时吊销              | SigV4 签名验证 + 最小权限策略                          |
| **能否简化**     | **可以极简**：单用户、无注册、无 OAuth              | **不可简化**：必须验证签名，拒绝未签名或签名错误的请求 |

S3 客户端（SDK、命令行工具、应用程序）本来就不走浏览器登录，它们天生使用 Access Key 进行 SigV4 签名。因此，**管理端的 Session 登录不会成为 S3 数据面的安全短板**，两者可以各自用最合适的方式实现。

---

### 🪶 管理端登录：极简 Cookie Session

2026 年对于自托管 SPA 的共识仍然是：**HttpOnly Cookie + 服务端 Session 是默认首选**，因为它能即时吊销，而 JWT 的吊销是固有难题。对于你这个轻量 OSS，管理端用户可能只有你自己或极少数运维人员，完全不需要引入 JWT 或外部身份提供商。

#### 用户模型：单用户或极小用户表

在 redb 中维护一张 `admin_users` 表即可，字段包括 `username`、`password_hash`、`created_at`。第一版甚至可以只支持**单用户**，通过环境变量或初始化命令设置管理员密码，彻底避免注册、找回密码等攻击面。

#### 密码哈希：Argon2id + OWASP 默认参数

不要自己调用底层的 `argon2` crate 去调参。使用 `salting` crate，它直接提供符合 OWASP Password Storage Cheat Sheet 的默认值：**64 MiB 内存、3 次迭代、4 个并行线程**，并强制使用 Argon2id。如果需要 pepper（服务端密钥），它也原生支持，可以在数据库意外泄露时让离线破解变得不可行。对于单用户场景，pepper 可以通过环境变量注入，进一步抬高攻击门槛。

```rust
use salting::{hash_password, verify_password};

let hash = hash_password("your-admin-password").unwrap();
// 登录时
assert!(verify_password("your-admin-password", &hash).unwrap());
```

#### 会话存储：tower-sessions + redb 自定义 Store

不需要引入 Redis。`axum-login` 底层使用 `tower-sessions` 做会话管理，而 `tower-sessions` 允许自定义 Store。既然元数据已经使用 redb，直接实现一个极简的 **redb Session Store**（一张 `sessions` 表，键为 Session ID，值为序列化的会话数据 + 过期时间），就能在单二进制内完成会话持久化，进程重启后会话仍然有效。

`axum-login` 提供了 `AuthUser` 和 `AuthnBackend` 两个 trait，你只需要实现“根据用户名查 redb 中的用户、用 `salting` 验证密码”这一小段逻辑即可。

#### Cookie 安全属性（必须全部设置）

```text
Set-Cookie: session_id=...; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=86400
```

- **HttpOnly**：前端 JavaScript 无法读取，XSS 无法直接窃取 Session。
- **Secure**：仅通过 HTTPS 传输。这意味着你**必须启用 TLS**，不能“先 HTTP 跑起来再说”。
- **SameSite=Strict**：管理端是纯 SPA 同源应用，Strict 可以阻断绝大多数 CSRF 场景，且不影响 S3 数据面（S3 请求不走 Cookie）。

#### 路由隔离：`/admin/*` 与管理 API 显式注册

管理端登录保护的路由是 `/admin/*`（或 `/api/admin/*`）。在 Axum 中，对这些路径挂上 `axum-login` 的 `login_required` 中间件，未认证请求直接返回 401 或重定向到登录页。**S3 端点（`/` 或 `/s3/*`）完全不经过 Session 中间件**，它们只经过 SigV4 验证中间件。

---

### 🛡️ S3 数据面：SigV4 + 最小权限，这才是“数据安全”的核心

管理端登录保护的是“谁可以改配置”，而**数据本身的安全必须由 SigV4 和访问策略来保证**。即使管理端 Session 被攻破，如果没有有效的 Access Key 和 Secret Key，攻击者仍然无法读写 Object。

#### SigV4 验证：使用 Sans-IO 的纯 Rust 实现

`rusty-s3` 是一个 Sans-IO 的纯 Rust S3 签名/验证库，已经过 MinIO 兼容性测试。它提供的是**签名逻辑**，而不是一个完整的服务端框架，正好适合嵌入你的 Axum 中间件：从请求中提取 `Authorization` 头、`x-amz-date`、`x-amz-content-sha256` 等字段，用存储的 Secret Key 重新计算签名并比对。

```rust
// 伪代码：在 Axum 中间件中验证 SigV4
async fn sigv4_auth_middleware(req: Request, next: Next) -> Result<Response, StatusCode> {
    let auth_header = req.headers().get("authorization")?;
    // 解析 Credential、SignedHeaders、Signature
    // 根据 Access Key ID 从 redb 查出对应的 Secret Key
    // 用 rusty-s3 或 reqsign-aws-v4 重新计算签名
    // 比对，失败返回 403
    next.run(req).await
}
```

`reqsign_aws_v4` 是另一个选择，同样提供纯 Rust 的 SigV4 签名能力。两者都可以，`rusty-s3` 的文档和测试更贴近 S3 服务端场景。

#### 访问策略：第一版用“Bucket → 允许的 Access Key”映射

不需要在第一版实现完整的 IAM 策略引擎。在 redb 中维护一张 `bucket_acl` 表，记录每个 Bucket 允许哪些 Access Key 进行读/写。SigV4 验证通过后，再检查该 Access Key 是否有目标 Bucket 的对应权限。这比完整的 IAM 简单两个数量级，但对于“保证数据安全”已经足够：**没有授权的 Key 无法触碰 Bucket**。

#### Secret Key 的存储：绝不明文落库

Access Key 的 Secret Key 在数据库中**不能明文存储**。推荐用**信封加密**：用一个主密钥（Master Key，来自环境变量或操作系统密钥链）加密每个 Secret Key 后再存入 redb。主密钥不落盘，进程启动时从环境变量加载。这样即使 redb 文件泄露，攻击者也无法直接还原 Secret Key。

#### Presigned URL：临时分享的安全通道

如果你需要临时分享 Object，不要生成永久公开链接。基于 SigV4 实现 **Presigned URL** 生成（`rusty-s3` 的 `action.sign(duration)` 直接支持），有效期设为分钟级（如 5–15 分钟）。Presigned URL 在有效期内无需额外凭证即可访问，过期自动失效，**不会暴露长期 Secret Key**，是对象存储中最轻量且安全的临时授权方式。

---

### 🔒 TLS：不可省略，但可以做得轻量

`Secure` Cookie 和 SigV4 都要求 HTTPS，否则凭证和签名头可能在明文传输中被截获。对于轻量部署，有两种 TLS 路径：

**路径 A：自签名证书（单机/内网场景）**

使用 `rcgen` 在首次启动时自动生成自签名证书，绑定 `localhost` 和 `127.0.0.1`。这适合本地测试或受控内网。但浏览器和 S3 客户端默认不信任自签名证书，需要在**安装脚本中引导用户将证书导入系统信任存储**：Linux 放入 `/usr/local/share/ca-certificates/` 后 `update-ca-certificates`；Windows 使用 `Import-Certificate` 导入到 `Cert:\LocalMachine\Root`。**必须导入“本地计算机”的“受信任的根证书颁发机构”存储**，导入到“当前用户”往往对服务无效。

**路径 B：Let‘s Encrypt（公网域名场景）**

如果服务器有公网域名，用 `rustls-acme` 在进程内自动申请和续期证书，完全无外部依赖。这是生产环境的推荐路径。

**路径 C：反向代理终止 TLS（最省心）**

如果你已经有 Nginx/Caddy，让它们终止 TLS，Rust 服务只监听 `127.0.0.1` 的 HTTP 端口。但此时 `Secure` Cookie 要求反向代理正确传递 `X-Forwarded-Proto: https`，Axum 侧需要信任代理头来设置 Secure 标志。

---

### 🏗️ 优化后的架构变化（仅增量）

相比原方案，改动集中在**网关层**，存储层和数据层完全不变：

```text
┌─────────────────────────────────────────────┐
│  Axum Server（TLS）                          │
│  ┌──────────────────────────────────────┐   │
│  │ /admin/*  → Session Cookie 中间件      │   │
│  │           → axum-login + redb Store   │   │
│  ├──────────────────────────────────────┤   │
│  │ S3 端点   → SigV4 验证中间件           │   │
│  │           → Bucket ACL 检查            │   │
│  ├──────────────────────────────────────┤   │
│  │ /（前端）  → 嵌入的 Vue3 SPA            │   │
│  └──────────────────────────────────────┘   │
├─────────────────────────────────────────────┤
│  Auth 模块（新增）                            │
│  · salting（Argon2id 密码哈希）              │
│  · redb Session Store（tower-sessions）      │
│  · SigV4 验证（rusty-s3 / reqsign-aws-v4）   │
│  · Secret Key 信封加密                        │
├─────────────────────────────────────────────┤
│  MetaEngine（redb）                          │
│  · objects / buckets                         │
│  · admin_users（新增）                       │
│  · sessions（新增）                          │
│  · bucket_acl（新增）                        │
│  · encrypted_secrets（新增）                 │
├─────────────────────────────────────────────┤
│  DataEngine（本地文件系统，流式 I/O）          │
└─────────────────────────────────────────────┘
```

**路由顺序必须精确**：`/admin/*` 的 Session 中间件和 S3 端点的 SigV4 中间件要显式挂在各自的路由分支上，不能让任何一方落入 SPA fallback。S3 路径（`/*bucket/*key` 风格）在 Axum 中可以通过 `route("/{bucket}/{*key}", ...)` 注册，与 `/admin` 前缀天然隔离。

---

### ✅ 安全底线清单（第一版必须全部满足）

| 检查项             | 具体要求                                                     |
| :----------------- | :----------------------------------------------------------- |
| **管理端密码**     | Argon2id + OWASP 默认参数（`salting` crate），禁止明文或弱哈希 |
| **管理端 Session** | `HttpOnly; Secure; SameSite=Strict`，服务端可即时吊销        |
| **S3 请求**        | **必须**验证 SigV4 签名，拒绝未签名/签名错误请求             |
| **S3 Secret Key**  | 信封加密后存储，主密钥来自环境变量，不落盘                   |
| **Bucket 访问**    | 最小权限：每个 Access Key 只允许访问被显式授权的 Bucket      |
| **TLS**            | 必须启用，`Secure` Cookie 和 SigV4 头依赖加密传输            |
| **临时分享**       | 使用短有效期 Presigned URL，不生成永久公开链接               |

这个方案没有引入任何外部服务（Redis、数据库、身份提供商），所有新增逻辑都收敛在单二进制内的 redb 表中。管理端登录简单到几乎“无感”，但 S3 数据面的安全强度没有打任何折扣——**SigV4 和最小权限才是对象存储数据安全的真正基石**。