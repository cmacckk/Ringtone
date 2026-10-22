# 给 NSIS 安装包做代码签名（顺便验签）
#
# 用法（PowerShell 7 / pwsh）：
#   # ① 证书装在 Windows 证书存储里（推荐，导不进 pfx 也能用）
#   .\scripts\sign-installer.ps1 -Thumbprint 1A2B3C...
#
#   # ② 手里是 pfx 文件
#   $env:CSC_KEY_PASSWORD = "证书密码"        # 只放当前会话
#   .\scripts\sign-installer.ps1 -Pfx D:\cert\ringtone.pfx
#
#   # ③ 只验一下现有签名
#   .\scripts\sign-installer.ps1 -VerifyOnly
#
# 默认签的是 src-tauri\target\release\bundle\nsis\Ringtone_<版本>_x64-setup.exe；
# 也可以 -File 指定任意 exe（比如 src-tauri\target\release\ringtone.exe）。
#
# 注意：只有「受信任的证书」才消得掉 SmartScreen 的「未知发布者」。
#   自己生成的自签证书（New-SelfSignedCertificate）能签、能验，但别人装上照样报警，
#   只适合内部试用（要先把 .cer 导进「受信任的根证书颁发机构」）。

param(
  [string]$Thumbprint = "",
  [string]$Pfx = "",
  [string]$PfxPassword = $env:CSC_KEY_PASSWORD,
  [string]$File = "",
  [string]$TimestampUrl = "http://timestamp.digicert.com",
  [switch]$VerifyOnly
)

$ErrorActionPreference = "Stop"

# 安装包路径：没给就按 tauri.conf.json 里的版本号拼
function Resolve-Installer([string]$path) {
  if ($path) { return (Resolve-Path $path).Path }
  $conf = "src-tauri\tauri.conf.json"
  if (-not (Test-Path $conf)) { throw "找不到 $conf（请在仓库根目录跑）" }
  $ver = (Get-Content $conf -Raw -Encoding UTF8 | ConvertFrom-Json).version
  $guess = "src-tauri\target\release\bundle\nsis\Ringtone_${ver}_x64-setup.exe"
  if (-not (Test-Path $guess)) { throw "找不到安装包：$guess（先跑 pnpm tauri build，或用 -File 指定）" }
  return (Resolve-Path $guess).Path
}

# signtool 在 Windows SDK 里，自己找最新那个 x64 的
function Find-SignTool {
  $roots = @("$env:ProgramFiles\Windows Kits\10\bin", "${env:ProgramFiles(x86)}\Windows Kits\10\bin")
  $hits = foreach ($r in $roots) {
    if (Test-Path $r) { Get-ChildItem $r -Directory | Sort-Object Name -Descending | ForEach-Object { Join-Path $_.FullName "x64\signtool.exe" } }
  }
  $hit = $hits | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $hit) { throw "找不到 signtool.exe：装一下 Windows SDK（或 Visual Studio 的「Windows 10/11 SDK」组件）" }
  return $hit
}

$signtool = Find-SignTool
$target = Resolve-Installer $File
$item = Get-Item $target
Write-Host "安装包：$($item.FullName)"
Write-Host "  大小：$([math]::Round($item.Length / 1MB, 2)) MiB"
Write-Host "  SHA256（签名前）：$((Get-FileHash $item.FullName -Algorithm SHA256).Hash.ToLower())"
Write-Host "signtool：$signtool"

if ($VerifyOnly) {
  Write-Host "`n== 验签 =="
  & $signtool verify /pa /v $item.FullName
  if ($LASTEXITCODE -ne 0) { throw "验签失败（退出码 $LASTEXITCODE）：这个文件没有可信签名" }
  Write-Host "验签通过 ✔"
  exit 0
}

# 签名参数
$signArgs = @("sign", "/fd", "sha256", "/td", "sha256")
if ($TimestampUrl) { $signArgs += @("/tr", $TimestampUrl) }
if ($Thumbprint) {
  $signArgs += @("/sha1", ($Thumbprint -replace '\s', ''))
  Write-Host "用证书存储里的证书：$Thumbprint"
} elseif ($Pfx) {
  if (-not (Test-Path $Pfx)) { throw "找不到证书文件：$Pfx" }
  $signArgs += @("/f", (Resolve-Path $Pfx).Path)
  if ($PfxPassword) { $signArgs += @("/p", $PfxPassword) }
  Write-Host "用证书文件：$Pfx"
} else {
  Write-Host @"

缺证书。三条路（都得你自己办，我没法代办）：

  1) 商业代码签名证书（OV / EV）—— DigiCert / Sectigo / GlobalSign 等，约 200~500 美元/年，
     要过个人或企业实名审核（几天到两三周）。EV 证书有即时信誉，SmartScreen 立刻不拦；
     OV 证书要攒下载量才慢慢好转。
  2) 微软 Azure Trusted Signing —— 约 10 美元/月，需要 Azure 订阅 + 身份验证
     （个人主体目前只对部分国家/地区开放），之后用 sign /dlib /dmdf 参数签，本脚本要小改。
  3) SignPath Foundation —— 对开源项目免费签名，要向它申请、通过审核。

自签证书（New-SelfSignedCertificate）能签能验，但别人装上照样「未知发布者」，
不建议当正式签名用。

拿到证书后二选一：
  证书装进「当前用户 → 个人 → 证书」 → .\scripts\sign-installer.ps1 -Thumbprint <指纹>
  拿到 .pfx 文件                     → `$env:CSC_KEY_PASSWORD='密码'; .\scripts\sign-installer.ps1 -Pfx <路径>

"@
  exit 2
}

Write-Host "`n== 签名 =="
& $signtool @signArgs $item.FullName
if ($LASTEXITCODE -ne 0) { throw "签名失败（退出码 $LASTEXITCODE）" }

Write-Host "`n== 验签 =="
& $signtool verify /pa /v $item.FullName
if ($LASTEXITCODE -ne 0) {
  throw "签名完了但验签没过（退出码 $LASTEXITCODE）。两种常见原因：`n" +
    "  · 用的是自签证书（或根证书没进「受信任的根证书颁发机构」）—— 签名本身已经嵌进去了，`n" +
    "    但别人那台机器不信任这条链，SmartScreen 照样会提示；换商业证书才能根治。`n" +
    "  · 商业证书但中间证书没带上 —— 用 /ac 参数把中间证书文件一并传进去。"
}

Write-Host "`n== 签名信息 =="
$sig = Get-AuthenticodeSignature $item.FullName
Write-Host "状态：$($sig.Status)  签名者：$($sig.SignerCertificate.Subject)"
Write-Host "时间戳：$($sig.TimeStamperCertificate.Subject)"
Write-Host "SHA256（签名后）：$((Get-FileHash $item.FullName -Algorithm SHA256).Hash.ToLower())"
Write-Host "`n✔ 已签名。签名会改文件字节，记得把新的 SHA256 同步到 README / release 说明里。"
