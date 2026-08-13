# celagent Windows 安装: 从 GitHub Release 下载 celagent-windows-x64.exe
# 上游 celld 暂无 Windows 包, 本脚本只装 CLI; 会话仍可走 BOS(需自行配置 aws CLI + [bos] profile)
# 用法: irm https://github.com/hxddh/celagent/releases/latest/download/install.ps1 | iex
$ErrorActionPreference = "Stop"
$Release = if ($env:CELAGENT_RELEASE_URL) { $env:CELAGENT_RELEASE_URL } else { "https://github.com/hxddh/celagent/releases/latest/download" }
$Root = if ($env:CELAGENT_ROOT) { $env:CELAGENT_ROOT } else { Join-Path $env:LOCALAPPDATA "celagent" }
$Bin = Join-Path $Root "bin"
New-Item -ItemType Directory -Force -Path $Bin | Out-Null
$Exe = Join-Path $Bin "celagent.exe"
Write-Host "下载 celagent-windows-x64.exe ..."
Invoke-WebRequest -Uri "$Release/celagent-windows-x64.exe" -OutFile $Exe -UseBasicParsing
Write-Host "已安装 $Exe"
Write-Host "把 $Bin 加入 PATH 后运行: celagent"
Write-Host "注意: 上游 celld 无 Windows 包; 持久化需 AWS CLI 的 [bos] profile。"
