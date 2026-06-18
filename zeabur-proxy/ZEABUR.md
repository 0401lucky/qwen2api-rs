# Zeabur 旁路代理部署指南

## 部署步骤

### 1. Zeabur 新建 Service

在同一个 Zeabur 项目中，**Add Service** → **Git**，连接仓库 `0401lucky/qwen2api-rs`。

设置：
- **Root Directory**: `zeabur-proxy`
- **Service Name**: 比如填 `mihomo`（记下这个名字，下一步要用）

### 2. 设置环境变量

在这个新 Service 的 Environment Variables 中添加：

```
SUB_URL=https://em.mesl.cloud/ems/get?token=fc38c85d9adca09cc31ae5ff3ec198ef
```

> ⚠️ 订阅链接含 token，不要在公开仓库里暴露。

### 3. 配置 qwen2api-rs 走代理

在 qwen2api-rs 的 Service 中添加环境变量：

```
UPSTREAM_PROXY=http://mihomo:7890
```

（如果你在步骤 1 取了其他名字，把 `mihomo` 换成你的 Service 名）

### 4. 验证

重新部署 qwen2api-rs，日志应该显示：

```
[QwenClient] 出口代理已启用（UPSTREAM_PROXY/运行时配置）
```

而不是之前的「出口代理未启用，已强制直连」。预热池也不会再报 WAF 错误。
