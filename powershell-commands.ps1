param (
    [Parameter(Mandatory=$true, Position=0)]
    [ValidateSet("prod", "dev")]
    [string]$Mode
)

$Port = "4000"
$LocalUrl = "http://localhost:$Port/"
$WorkingDir = "rust-server"

Write-Host "Building TypeScript..." -ForegroundColor Green
npm.cmd run build

if ($LASTEXITCODE -ne 0) {
    Write-Host "TypeScript build failed. Aborting." -ForegroundColor Red
    exit $LASTEXITCODE
}

if ($Mode -eq "prod") {
    Write-Host "Starting MercTest in PRODUCTION mode..." -ForegroundColor Yellow
    $env:IS_PROD = "true"
}
else {
    Write-Host "Starting MercTest in DEV mode..." -ForegroundColor Cyan
    $env:IS_PROD = "false"
}

$LanIp = Get-NetIPAddress -AddressFamily IPv4 |
    Where-Object {
        $_.IPAddress -notmatch "^(127\.|169\.254\.)" -and
        $_.PrefixOrigin -ne "WellKnown"
    } |
    Select-Object -First 1 -ExpandProperty IPAddress

Write-Host "Local URL: $LocalUrl" -ForegroundColor DarkCyan

if ($LanIp) {
    Write-Host "LAN URL: http://${LanIp}:$Port/" -ForegroundColor DarkCyan
}

$cargoParams = @{
    FilePath         = "cargo"
    ArgumentList     = "run", "--manifest-path", "Cargo.toml"
    WorkingDirectory = $WorkingDir
    NoNewWindow      = $true
    Wait             = $true
}

Start-Process @cargoParams
