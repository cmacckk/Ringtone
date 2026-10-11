# 发布 v0.1.0：建 release + 上传安装包
#
# 用法（PowerShell，在本机跑；需要一把有 repo 权限的 GitHub token）：
#   cd F:\develop\Ringtone
#   $env:GH_TOKEN = "ghp_xxx"          # 只放当前会话，别写进文件
#   .\scripts\publish-release.ps1
#
# 也可以传参数：
#   .\scripts\publish-release.ps1 -Token "ghp_xxx" -Tag v0.1.0
#
# 脚本做三件事：① 建 tag/release（已存在就复用）② 上传安装包 ③ 打印结果链接。
# 全程只调用 GitHub REST API，不用 gh CLI（本机没装）。

param(
  [string]$Token = $env:GH_TOKEN,
  [string]$Repo = "cmacckk/Ringtone",
  [string]$Tag = "v0.1.0",
  [string]$Target = "",   # 空 = 用仓库的默认分支（别写死 master：仓库本地叫 master、远端可能叫别的）
  [string]$Installer = "src-tauri\target\release\bundle\nsis\Ringtone_0.1.0_x64-setup.exe"
)

$ErrorActionPreference = "Stop"
if (-not $Token) { throw "缺少 token：先 `$env:GH_TOKEN = ""ghp_xxx""，或用 -Token 传进来" }
if (-not (Test-Path $Installer)) { throw "找不到安装包：$Installer（先跑 `pnpm tauri build`）" }

$headers = @{ Authorization = "Bearer $Token"; "User-Agent" = "ringtone-release" }
$api = "https://api.github.com/repos/$Repo"
$file = Get-Item $Installer
$sum = (Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLower()

# 目标分支：默认用仓库的默认分支（远端不一定有 master）
if (-not $Target) {
  $Target = (Invoke-RestMethod -Uri $api -Headers $headers).default_branch
  Write-Host "target_commitish = $Target（仓库默认分支）"
}

# ① release：存在就复用，不存在就建
$release = $null
try {
  $release = Invoke-RestMethod -Uri "$api/releases/tags/$Tag" -Headers $headers
  Write-Host "release $Tag 已存在（id=$($release.id)），直接上传"
} catch {
  $body = @{
    tag_name         = $Tag
    target_commitish = $Target
    name             = "Ringtone $Tag"
    body             = @"
本地优先的定时提醒 + 多平台推送桌面应用（Windows 10/11）。

**下载** Ringtone_0.1.0_x64-setup.exe 双击安装（当前用户安装，不需要管理员；需要 WebView2，Win11 与较新的 Win10 已自带）。安装包没有代码签名，SmartScreen 拦一下 —— 点「更多信息」→「仍要运行」。

**能干什么**

- 按天规划待办，可重复（每天 / 工作日 / 每周 / 每月 / 每月最后工作日 / 每年，支持「从…重复到…」区间）
- 到点把提醒推到**钉钉 / 飞书**；主渠道失败自动降级，全挂了用系统通知兜底
- 「工作日」认中国法定节假日（法定节假日不响、调休补班的周末照响）
- 中文快捷录入（例如：明天九点交周报）、多次提前提醒、按任务选推送渠道
- 贴边浮窗（可勾完成 / 换边 / 独立字体字号）、全局快捷键与鼠标侧键、开机自启、上下班提醒
- 导入导出 Markdown / CSV、月总览、深/浅主题、背景图

数据只存在本机 %APPDATA%\cn.ringtone.desktop（不用登录、不联网同步）。

SHA256：$sum
"@
    draft            = $false
    prerelease       = $false
  } | ConvertTo-Json -Depth 4
  $release = Invoke-RestMethod -Method Post -Uri "$api/releases" -Headers $headers -ContentType "application/json" -Body $body
  Write-Host "已建 release $Tag（id=$($release.id)）"
}

# ② 上传安装包（同名 asset 先删掉，免得 422）
$assets = Invoke-RestMethod -Uri "$api/releases/$($release.id)/assets" -Headers $headers
foreach ($a in $assets) {
  if ($a.name -eq $file.Name) {
    Invoke-RestMethod -Method Delete -Uri "$api/releases/assets/$($a.id)" -Headers $headers
    Write-Host "删掉旧的同名 asset：$($a.name)"
  }
}
$upload = "$($release.upload_url -replace '\{.*\}', '')?name=$($file.Name)"
$asset = Invoke-RestMethod -Method Post -Uri $upload -Headers $headers -ContentType "application/octet-stream" -InFile $file.FullName
Write-Host "已上传：$($asset.browser_download_url)"
Write-Host "大小：$([math]::Round($file.Length / 1MB, 2)) MiB"
Write-Host "SHA256：$sum"
Write-Host "release 页面：$($release.html_url)"
