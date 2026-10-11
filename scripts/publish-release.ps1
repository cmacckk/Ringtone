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
  [string]$Installer = "src-tauri\target\release\bundle\nsis\Ringtone_0.1.0_x64-setup.exe"
)

$ErrorActionPreference = "Stop"
if (-not $Token) { throw "缺少 token：先 `$env:GH_TOKEN = ""ghp_xxx""，或用 -Token 传进来" }
if (-not (Test-Path $Installer)) { throw "找不到安装包：$Installer（先跑 `pnpm tauri build`）" }

$headers = @{ Authorization = "Bearer $Token"; "User-Agent" = "ringtone-release" }
$api = "https://api.github.com/repos/$Repo"
$file = Get-Item $Installer
$sum = (Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLower()

# ① release：存在就复用，不存在就建
$release = $null
try {
  $release = Invoke-RestMethod -Uri "$api/releases/tags/$Tag" -Headers $headers
  Write-Host "release $Tag 已存在（id=$($release.id)），直接上传"
} catch {
  $body = @{
    tag_name         = $Tag
    target_commitish = "master"
    name             = "Ringtone $Tag"
    body             = "首个可用版本：本地待办 + 到点推送到钉钉 / 飞书。`n`n下载 `Ringtone_0.1.0_x64-setup.exe` 双击安装（Windows 10/11，需要 WebView2，Win11 自带）。`n`nSHA256：``$sum``"
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
