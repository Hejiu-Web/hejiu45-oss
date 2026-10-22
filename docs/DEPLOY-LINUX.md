# Linux 部署指南

本文档基于实际部署验证，涵盖 hejiu45-oss 在 Linux 上的完整部署流程：从下载校验到 systemd 常驻，再到反向代理和 HTTPS。

**适用版本**：v0.2.0 及以上
**验证环境**：CentOS 8.5 x86_64 + Nginx + frp 内网穿透

---

## 目录

1. [安装前确认](#1-安装前确认)
2. [方式一：一键脚本](#2-方式一一键脚本)
3. [方式二：手动部署（推荐）](#3-方式二手动部署推荐)
4. [配置反向代理](#4-配置反向代理)
5. [常用运维命令](#5-常用运维命令)
6. [备份与恢复](#6-备份与恢复)
7. [升级](#7-升级)
8. [故障排查](#8-故障排查)
9. [附录：完整参数说明](#9-附录完整参数说明)

---

## 1. 安装前确认

```bash
# 系统与架构
cat /etc/os-release | head -3
uname -m                    # 应为 x86_64 或 aarch64

# 端口占用（默认用 9000）
ss -lntp | grep -E ':(9000)\b'

# 已安装？
systemctl status hejiu45-oss --no-pager
```

### 系统要求

| 项 | 要求 |
| :-- | :-- |
| 架构 | x86_64 / aarch64 |
| glibc | **无要求**——二进制是 musl 静态链接 |
| 磁盘 | 按预期存储量规划；元数据约 1 MB 起步 |
| 内存 | 最小 100 MB，实际运行通常 < 10 MB |

> **CentOS 7 及更早**：静态二进制本身能跑，但 systemd 版本较旧。脚本未在此类系统上测试，建议用手动部署。
>
> **CentOS 8 / Stream 8**：已于 2021-12-31 EOL，软件源已下线。安装任何包前必须先修源（见 [4.1](#41-centos-8-修源)）。

---

## 2. 方式一：一键脚本

```bash
curl -fsSL https://raw.githubusercontent.com/Hejiu-Web/hejiu45-oss/main/install-linux.sh | sudo bash
```

**脚本行为**（逐条对应源码）：

1. `uname -m` 识别架构（x86_64→amd64，aarch64→arm64）
2. 从 latest release 下载匹配的静态二进制
3. 拉取 `checksums-hejiu45-oss.txt` 并**校验 SHA-256**，不匹配则中止
4. 安装到 `/usr/local/bin/hejiu45-oss`
5. 创建系统用户 `hejiu45-oss` 和数据目录 `/var/lib/hejiu45-oss`
6. 生成 master key（权限 0600）
7. 写入加固的 systemd 单元并启动
8. 打印初始管理员密码

### 可选环境变量

| 变量 | 默认 | 说明 |
| :-- | :-- | :-- |
| `HEJIU45_OSS_VERSION` | `latest` | 指定版本，如 `v0.2.0` |
| `HEJIU45_OSS_LISTEN` | `127.0.0.1:9000` | 监听地址 |
| `HEJIU45_OSS_TLS_MODE` | `reverse-proxy` | `off` / `manual` / `reverse-proxy` |
| `HEJIU45_OSS_TLS_CERT` | — | `manual` 模式的证书路径 |
| `HEJIU45_OSS_TLS_KEY` | — | `manual` 模式的私钥路径 |
| `HEJIU45_OSS_DOMAIN` | — | 公网地址，如 `https://oss.example.com` |
| `HEJIU45_OSS_ADMIN_PASSWORD` | 自动生成 | 初始密码 |
| `HEJIU45_OSS_NGINX` | — | 设为 `1` 同时安装 Nginx 站点 |
| `HEJIU45_OSS_UNINSTALL` | — | 设为 `1` 卸载（保留数据） |

用法示例：

```bash
HEJIU45_OSS_LISTEN=127.0.0.1:9100 \
HEJIU45_OSS_DOMAIN=https://oss.example.com \
  curl -fsSL https://raw.githubusercontent.com/Hejiu-Web/hejiu45-oss/main/install-linux.sh | sudo bash
```

> **限制**：脚本不支持自定义安装路径（`BIN`/`DATA_DIR` 是写死的常量）。需要自定义路径请用下面的手动部署。

---

## 3. 方式二：手动部署（推荐）

手动部署透明可控，且能自由指定路径。静态二进制没有"安装"过程，本质就是 `cp` + 写一个 systemd 单元。

### 3.1 下载并校验

```bash
cd /tmp

# 下载二进制（ARM 换成 hejiu45-oss-linux-arm64）
curl -fLO https://github.com/Hejiu-Web/hejiu45-oss/releases/latest/download/hejiu45-oss-linux-amd64

# 下载校验文件
curl -fsLO https://github.com/Hejiu-Web/hejiu45-oss/releases/latest/download/checksums-hejiu45-oss.txt

# 校验（必须看到 OK）
sha256sum -c --ignore-missing checksums-hejiu45-oss.txt
```

期望输出：

```
hejiu45-oss-linux-amd64: OK
```

**校验不通过就不要继续。** 这是唯一能确认二进制完整性的环节。

> **无法直连 GitHub 时**：在浏览器下载对应文件，用 `scp` 上传到 `/tmp`，然后同样执行 `sha256sum -c`。

### 3.2 安装二进制

以下示例使用 `/data_source/hejiu45-oss`，可自行替换。

```bash
# 创建目录结构
mkdir -p /data_source/hejiu45-oss/bin /data_source/hejiu45-oss/data

# 安装二进制
install -m0755 /tmp/hejiu45-oss-linux-amd64 /data_source/hejiu45-oss/bin/hejiu45-oss

# 验证能跑
/data_source/hejiu45-oss/bin/hejiu45-oss --version
```

输出 `hejiu45-oss 0.2.0` 即成功。

### 3.3 创建服务用户与授权

程序以非 root 用户运行，减少攻击面。

```bash
useradd --system --no-create-home --shell /sbin/nologin hejiu45-oss

# 授权（必须：否则服务无法写数据目录）
chown -R hejiu45-oss:hejiu45-oss /data_source/hejiu45-oss
chmod 750 /data_source/hejiu45-oss/data
```

验证目录层级可进入：

```bash
namei -l /data_source/hejiu45-oss/data
```

每一级都要对 `hejiu45-oss` 有 `x` 权限。

### 3.4 首次启动（获取初始密码）

> **关键**：这一步必须在前台运行，因为初始密码只在首次启动时打印一次。

```bash
# 确认没有残留进程（有就 kill 掉）
ps -ef | grep hejiu45 | grep -v grep

sudo -u hejiu45-oss /data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --listen 127.0.0.1:9000 \
  --tls reverse-proxy \
  --public-url https://oss.example.com
```

看到如下输出：

```
INFO starting version="0.2.0" product="hejiu45-oss"
WARN generated a master key; ... path=.../master.key
──────────────────────────────────────────────
🔐 Initial admin credentials for hejiu45-oss
┌──────────────────────────────────────────┐
│ username: admin                          │
│ password: xxxxxxxxxxxxxxxxxxxxxxxxxxxx   │
└──────────────────────────────────────────┘
This password is shown once. Change it after your first login.
──────────────────────────────────────────────
INFO console ready url=http://127.0.0.1:9000
INFO serving HTTP addr=127.0.0.1:9000
```

**立即复制密码保存。** 然后按 `Ctrl+C` 停止。

> **想自己指定密码**：加 `--admin-password "你的密码"`。指定后不会打印密码方框，日志显示 `created initial admin account`。
>
> **注意**：`--admin-password` **只在首次创建账号时生效**，对已存在的账号无效。

### 3.5 备份 master key

```bash
cp /data_source/hejiu45-oss/data/master.key /root/master.key.backup
chmod 600 /root/master.key.backup
```

**这一步不能跳过。** master key 用于加密存储 S3 访问密钥，丢失后所有已保存的密钥永久不可解密（对象数据本身不受影响）。

### 3.6 配置 systemd

```bash
tee /etc/systemd/system/hejiu45-oss.service > /dev/null <<'EOF'
[Unit]
Description=hejiu45-oss object storage
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=hejiu45-oss
Group=hejiu45-oss
ExecStart=/data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --listen 127.0.0.1:9000 \
  --tls reverse-proxy \
  --public-url https://oss.example.com
Restart=on-failure
RestartSec=5

# 安全加固
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/data_source/hejiu45-oss/data

# 大量并发上传/下载需要足够的文件描述符
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now hejiu45-oss
systemctl status hejiu45-oss --no-pager
```

### 3.7 验证

```bash
# 端口在监听
ss -lntp | grep :9000

# 健康检查
curl -fsS http://127.0.0.1:9000/healthz
# 期望：{"status":"ok","service":"hejiu45-oss"}
```

---

## 4. 配置反向代理

**不要把 `--listen` 改成 `0.0.0.0:9000` 后用明文 HTTP 暴露。** 服务只有在确认请求来自 HTTPS 时才给会话 Cookie 加 `Secure` 属性；明文环境下控制台等同于无保护。

### 4.1 CentOS 8 修源

CentOS 8 已 EOL，官方源已下线。**不修源则无法安装 Nginx**：

```bash
sed -i 's|mirrorlist=|#mirrorlist=|g' /etc/yum.repos.d/CentOS-*.repo
sed -i 's|#baseurl=http://mirror.centos.org|baseurl=http://vault.centos.org|g' /etc/yum.repos.d/CentOS-*.repo
dnf clean all && dnf makecache
```

验证：

```bash
dnf install -y epel-release
```

### 4.2 安装 Nginx

```bash
dnf install -y nginx
systemctl enable --now nginx
```

如需自动申请证书：

```bash
dnf install -y certbot python3-certbot-nginx
```

### 4.3 站点配置

新建 `/etc/nginx/conf.d/hejiu45-oss.conf`：

```nginx
# HTTP 跳转 HTTPS
server {
    listen 80;
    server_name oss.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    server_name oss.example.com;

    ssl_certificate      /etc/nginx/ssl/oss.example.com.pem;
    ssl_certificate_key  /etc/nginx/ssl/oss.example.com.key;
    ssl_protocols        TLSv1.2 TLSv1.3;
    ssl_ciphers          HIGH:!aNULL:!MD5;
    ssl_prefer_server_ciphers on;
    ssl_session_cache    shared:SSL:10m;
    ssl_session_timeout  10m;

    # ===== 对象存储必需的三项 =====
    client_max_body_size 0;          # 否则默认 1 MB，大文件返回 413
    proxy_request_buffering off;     # 否则大文件先缓存到 Nginx，可能爆盘
    proxy_buffering off;

    location / {
        proxy_pass http://127.0.0.1:9000;

        proxy_set_header Host              $host;
        proxy_set_header X-Real-IP         $remote_addr;
        proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
        # 关键：服务靠这个头判断请求是否为 HTTPS
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-Host  $host;
        proxy_set_header X-Forwarded-Port  443;

        proxy_http_version 1.1;
        proxy_set_header Upgrade    $http_upgrade;
        proxy_set_header Connection "upgrade";

        # 大文件传输需要长超时
        proxy_connect_timeout  60s;
        proxy_send_timeout    3600s;
        proxy_read_timeout    3600s;
    }

    error_page 500 502 503 504 /50x.html;
    location = /50x.html { root html; }
}
```

### 4.4 校验并重载

```bash
nginx -t
systemctl reload nginx
```

### 4.5 申请证书（可选）

```bash
certbot --nginx -d oss.example.com
```

### 4.6 开放防火墙

```bash
firewall-cmd --permanent --add-service=https
firewall-cmd --permanent --add-service=http    # certbot 验证用
firewall-cmd --reload
```

**注意**：后端端口 9000 只监听回环，**不需要**对外放行。

### 4.7 验证

```bash
# 本机验证（绕过 DNS）
curl -fsS https://oss.example.com/healthz --resolve oss.example.com:443:127.0.0.1

# 外部验证
curl -fsS https://oss.example.com/healthz
```

确认 Cookie 带 `Secure` 属性：

```bash
curl -sI -X POST https://oss.example.com/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"你的密码"}' | grep -i set-cookie
```

正确应包含 `Secure`。若没有，说明 `X-Forwarded-Proto` 没传到服务。

---

## 5. 常用运维命令

```bash
# 服务管理
systemctl status hejiu45-oss --no-pager
systemctl restart hejiu45-oss
systemctl stop hejiu45-oss
systemctl reload-or-restart hejiu45-oss

# 查看日志（实时）
journalctl -u hejiu45-oss -f

# 查看最近日志
journalctl -u hejiu45-oss -n 100 --no-pager

# 本次启动以来的日志
journalctl -u hejiu45-oss -b --no-pager

# 查看有效配置
/data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --print-config

# 健康检查
curl -fsS http://127.0.0.1:9000/healthz
```

### 数据目录结构

```
/data_source/hejiu45-oss/data/
├── meta.redb        # 元数据库（桶、对象索引、密钥、会话、审计）
├── master.key       # 主密钥（0600）—— 必须单独备份
├── objects/         # 对象数据，按 key 的 SHA-256 哈希分层存放
├── multipart/       # 分片上传的临时分片
├── tmp/             # 写入暂存区
└── server.lock      # 单实例锁
```

---

## 6. 备份与恢复

### 6.1 在线备份元数据

```bash
/data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --backup-to /root/backup/meta-$(date +%F).redb
```

`--backup-to` 使用 redb 的 savepoint 语义，**在服务运行时也能得到一致快照**。

### 6.2 完整备份

```bash
BACKUP_DIR=/root/backup/$(date +%F)
mkdir -p "$BACKUP_DIR"

# 元数据（在线一致快照）
/data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --backup-to "$BACKUP_DIR/meta.redb"

# 主密钥（必须）
cp /data_source/hejiu45-oss/data/master.key "$BACKUP_DIR/"

# 对象数据
tar -C /data_source/hejiu45-oss/data -czf "$BACKUP_DIR/objects.tgz" objects
```

### 6.3 恢复

```bash
systemctl stop hejiu45-oss

# 从备份恢复元数据到空目录
/data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --restore-from /root/backup/2026-10-22/meta.redb

# 恢复对象数据
tar -C /data_source/hejiu45-oss/data -xzf /root/backup/2026-10-22/objects.tgz

# 恢复主密钥（如果数据中心重建）
cp /root/backup/2026-10-22/master.key /data_source/hejiu45-oss/data/
chown hejiu45-oss:hejiu45-oss /data_source/hejiu45-oss/data/master.key
chmod 600 /data_source/hejiu45-oss/data/master.key

systemctl start hejiu45-oss
```

> `--restore-from` 拒绝覆盖非空数据目录。

---

## 7. 升级

```bash
# 1. 记录当前版本
/data_source/hejiu45-oss/bin/hejiu45-oss --version

# 2. 下载新版本并校验
cd /tmp
curl -fLO https://github.com/Hejiu-Web/hejiu45-oss/releases/latest/download/hejiu45-oss-linux-amd64
curl -fsLO https://github.com/Hejiu-Web/hejiu45-oss/releases/latest/download/checksums-hejiu45-oss.txt
sha256sum -c --ignore-missing checksums-hejiu45-oss.txt

# 3. 先备份
/data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --backup-to /root/backup/pre-upgrade-$(date +%F).redb

# 4. 停服务，替换二进制
systemctl stop hejiu45-oss
install -m0755 /tmp/hejiu45-oss-linux-amd64 /data_source/hejiu45-oss/bin/hejiu45-oss

# 5. 启动
systemctl start hejiu45-oss
systemctl status hejiu45-oss --no-pager

# 6. 验证
curl -fsS http://127.0.0.1:9000/healthz
```

**数据目录不需要改动**。新版启动时会自动补齐新增的表结构。

---

## 8. 故障排查

### 8.1 服务起不来

```bash
journalctl -u hejiu45-oss -n 50 --no-pager
```

常见原因：

| 日志关键字 | 原因 | 解决 |
| :-- | :-- | :-- |
| `another hejiu45-oss instance is already using` | 有残留进程占着数据目录 | `ps -ef \| grep hejiu45`，kill 掉 |
| `permission denied` | 数据目录属主不对 | `chown -R hejiu45-oss:hejiu45-oss <数据目录>` |
| `Read-only file system` | systemd 沙箱没放行 | 检查 `ReadWritePaths` 与 `--data-dir` 是否一致 |
| `Address already in use` | 端口冲突 | `ss -lntp \| grep :9000` 查占用者，或换端口 |

### 8.2 登录后立即跳回登录页

**症状**：能打开登录页、提交也成功，但立刻返回登录页。

**原因**：Cookie 没带 `Secure` 属性，或 `SameSite=Strict` 与跨域不匹配。

**排查**：

```bash
# 检查 systemd 里的 TLS 参数
grep ExecStart /etc/systemd/system/hejiu45-oss.service
```

必须是 `--tls reverse-proxy`（前面有代理）。如果写成了 `--allow-insecure`，Cookie 不会带 `Secure`。

```bash
# 检查 Cookie 属性
curl -sI -X POST https://oss.example.com/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"..."}' | grep -i set-cookie
```

再检查 Nginx 是否设置了 `X-Forwarded-Proto https`。

### 8.3 忘记管理员密码

**背景**：`--admin-password` 只在首次创建账号时生效，无法修改已存在的密码。

**解决**：删除元数据库重建账号（对象数据不受影响）。

```bash
# 1. 确认没有残留进程（关键！）
systemctl stop hejiu45-oss
ps -ef | grep hejiu45 | grep -v grep    # 必须无输出，有就 kill

# 2. 备份并删除元数据
cp /data_source/hejiu45-oss/data/meta.redb /root/meta.redb.bak
rm -f /data_source/hejiu45-oss/data/meta.redb

# 3. 确认删掉了
ls -l /data_source/hejiu45-oss/data/     # 不应有 meta.redb

# 4. 前台重建账号
sudo -u hejiu45-oss /data_source/hejiu45-oss/bin/hejiu45-oss \
  --data-dir /data_source/hejiu45-oss/data \
  --admin-user admin \
  --admin-password "新密码" \
  --listen 127.0.0.1:9999 --allow-insecure

# 必须看到：INFO created initial admin account username=admin

# 5. 另开终端验证
curl -s -X POST http://127.0.0.1:9999/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"新密码"}'
# 期望：{"ok":true,"username":"admin"}

# 6. 回到第 4 步终端 Ctrl+C，然后
systemctl start hejiu45-oss
```

> **代价**：桶、对象索引、访问密钥全部清空。`objects/` 里的文件不会删除，但会变成孤儿文件。**建议先确认没有业务数据再执行。**

### 8.4 上传大文件失败

| 现象 | 原因 | 解决 |
| :-- | :-- | :-- |
| 413 Request Entity Too Large | Nginx `client_max_body_size` 默认 1 MB | 设为 `0` |
| 传到一半断开 | Nginx 超时 | `proxy_send_timeout` / `proxy_read_timeout` 设为 `3600s` |
| 服务端提示超限 | 控制台设置里的大小限制 | 设置 → 上传限制，调大或勾选"不限制" |

### 8.5 无法从外部访问

```bash
# 1. 本机健康检查
curl -fsS http://127.0.0.1:9000/healthz

# 2. 本机 HTTPS（绕过 DNS）
curl -fsS https://oss.example.com/healthz --resolve oss.example.com:443:127.0.0.1

# 3. DNS 解析
dig +short oss.example.com

# 4. 防火墙
firewall-cmd --list-all

# 5. Nginx 错误日志
tail -50 /var/log/nginx/error.log
```

---

## 9. 附录：完整参数说明

### 核心参数

| 参数 | 环境变量 | 默认值 | 说明 |
| :-- | :-- | :-- | :-- |
| `--listen` | `HEJIU45_OSS_LISTEN` | `127.0.0.1:9000` | 监听地址 |
| `--data-dir` | `HEJIU45_OSS_DATA_DIR` | — | 数据目录（必需） |
| `--tls` | `HEJIU45_OSS_TLS` | `off` | `off` / `manual` / `reverse-proxy` |
| `--tls-cert` | `HEJIU45_OSS_TLS_CERT` | — | `manual` 模式证书 |
| `--tls-key` | `HEJIU45_OSS_TLS_KEY` | — | `manual` 模式私钥 |
| `--public-url` | `HEJIU45_OSS_PUBLIC_URL` | — | 公网地址，用于展示 endpoint 和预签名 URL |
| `--s3-prefix` | — | `/s3` | S3 API 挂载路径，空字符串表示根路径 |
| `--region` | `HEJIU45_OSS_REGION` | `us-east-1` | S3 区域标识 |

### 安全与认证

| 参数 | 环境变量 | 默认值 | 说明 |
| :-- | :-- | :-- | :-- |
| `--master-key` | `HEJIU45_OSS_MASTER_KEY` | 自动生成 | 32 字节主密钥（hex 或 base64） |
| `--admin-user` | `HEJIU45_OSS_ADMIN_USER` | `admin` | 初始管理员用户名 |
| `--admin-password` | `HEJIU45_OSS_ADMIN_PASSWORD` | 自动生成 | 初始密码，**仅首次生效** |
| `--session-hours` | `HEJIU45_OSS_SESSION_HOURS` | `12` | 控制台会话有效期 |
| `--allow-insecure` | `HEJIU45_OSS_ALLOW_INSECURE` | `false` | 允许明文（仅限本机调试） |
| `--trust-proxy` | `HEJIU45_OSS_TRUST_PROXY` | — | 信任来自回环的 `X-Forwarded-*` |
| `--trust-forwarded-for` | `HEJIU45_OSS_TRUST_FORWARDED_FOR` | — | 用 `X-Forwarded-For` 作为审计来源 IP |

### 运维

| 参数 | 说明 |
| :-- | :-- |
| `--backup-to <path>` | 导出元数据快照并退出 |
| `--restore-from <path>` | 从快照恢复到数据目录并退出 |
| `--init-access-key <label>` | 离线创建访问密钥并打印 AK/SK，然后退出 |
| `--print-config` | 打印有效配置并退出 |
| `--log <filter>` | 日志级别，如 `info`、`hejiu45_oss=debug` |

---

## 部署检查清单

部署完成后逐项确认：

- [ ] `hejiu45-oss --version` 输出正确版本
- [ ] `sha256sum -c` 校验通过
- [ ] `systemctl status` 显示 `active (running)` 且 `enabled`
- [ ] `curl http://127.0.0.1:9000/healthz` 返回 `{"status":"ok",...}`
- [ ] **`master.key` 已备份到数据目录之外**
- [ ] 浏览器能打开控制台并登录
- [ ] 登录后 Cookie 带 `Secure` 属性
- [ ] **已修改初始密码**
- [ ] Nginx `client_max_body_size 0` 已设置
- [ ] 上传测试：小文件、大文件（> 100 MB）均成功
- [ ] 外部网络可以访问
- [ ] 防火墙只开放 80/443，不开放 9000
