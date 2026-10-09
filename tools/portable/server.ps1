param(
    [ValidateSet('Start', 'Stop', 'Status', 'AllowFirewall', 'ExportCommunity')]
    [string]$Action = 'Start',
    [string]$ServerDirectory = $PSScriptRoot,
    [ValidateRange(1024, 65535)]
    [int]$Port = 4433,
    [string]$RelayAddress,
    [string]$ClientDirectory
)

$ErrorActionPreference = 'Stop'
$serverRoot = (Resolve-Path -LiteralPath $ServerDirectory).Path
$binary = Join-Path $serverRoot 'iw4l-master.exe'
$certificate = Join-Path $serverRoot 'server-cert.pem'
$privateKey = Join-Path $serverRoot 'server-key.pem'
$ca = Join-Path $serverRoot 'ca.pem'
$pidFile = Join-Path $serverRoot 'server.pid'
$ownedProcess = $null
if (Test-Path -LiteralPath $pidFile) {
    $serverPid = 0
    if ([int]::TryParse((Get-Content -Raw -LiteralPath $pidFile).Trim(), [ref]$serverPid)) {
        $candidate = Get-Process -Id $serverPid -ErrorAction SilentlyContinue
        if ($candidate -and $candidate.Path -eq $binary) { $ownedProcess = $candidate }
    }
}

switch ($Action) {
    'ExportCommunity' {
        if (!$ClientDirectory) { throw 'Set -ClientDirectory to the extracted player game folder.' }
        if (!$RelayAddress) {
            $tailscale = Join-Path $env:ProgramFiles 'Tailscale/tailscale.exe'
            if (!(Test-Path -LiteralPath $tailscale)) { throw 'Install and sign in to Tailscale, or supply -RelayAddress.' }
            $RelayAddress = (& $tailscale ip -4 | Select-Object -First 1)
            if ($LASTEXITCODE -ne 0 -or !$RelayAddress) { throw 'Sign in to Tailscale before exporting the player connection.' }
            $RelayAddress = $RelayAddress.Trim()
        }
        if ($RelayAddress -notmatch '^[A-Za-z0-9][A-Za-z0-9.-]*$') { throw 'RelayAddress must be an IPv4 address or hostname, without a port.' }
        $clientRoot = (Resolve-Path -LiteralPath $ClientDirectory).Path
        if (!(Test-Path -LiteralPath (Join-Path $clientRoot 'iw4l.exe'))) { throw 'ClientDirectory must contain iw4l.exe.' }
        $publicCa = (Get-Content -Raw -LiteralPath $ca).Trim()
        if ($publicCa -notmatch '^-----BEGIN CERTIFICATE-----' -or $publicCa -match 'PRIVATE KEY') { throw 'ca.pem must contain only public certificates.' }
        $descriptor = @"
schema = 1
name = "Friends Multiplayer"
[master]
address = "$($RelayAddress):$Port"
server_name = "iw4l-prod"
[updates]
auto_update = false
ca_pem = '''
$publicCa
'''
"@
        $destination = Join-Path $clientRoot 'friends.iw4l-server'
        [System.IO.File]::WriteAllText($destination, $descriptor)
        Write-Output "Player connection saved to $destination. Share this file with players using the same build."
    }
    'Status' {
        & $binary status --connect "127.0.0.1:$Port" --server-name iw4l-prod --ca-cert $ca
        if ($LASTEXITCODE -ne 0) { throw 'Relay is not responding.' }
    }
    'Stop' {
        if ($ownedProcess) { Stop-Process -Id $ownedProcess.Id }
        if (Test-Path -LiteralPath $pidFile) { Remove-Item -LiteralPath $pidFile }
    }
    'AllowFirewall' {
        $ruleName = "IW4L-Relay-UDP-$Port"
        $rule = Get-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue
        if ($rule) { throw "Firewall rule $ruleName already exists; inspect it before changing it." }
        New-NetFirewallRule -Name $ruleName -DisplayName "IW4L multiplayer relay UDP $Port" `
            -Direction Inbound -Action Allow -Protocol UDP -LocalPort $Port -Program $binary -Profile Any
    }
    'Start' {
        if ($ownedProcess) { Write-Output "Relay already running (PID $($ownedProcess.Id))."; break }
        foreach ($file in @($binary, $certificate, $privateKey, $ca)) {
            if (!(Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing $file" }
        }
        $updates = Join-Path $serverRoot 'updates'
        New-Item -ItemType Directory -Force -Path $updates | Out-Null
        $process = Start-Process -FilePath $binary -WindowStyle Hidden -PassThru `
            -ArgumentList @('serve', '--bind', "0.0.0.0:$Port", '--cert', ('"'+$certificate+'"'),
                '--key', ('"'+$privateKey+'"'), '--updates', ('"'+$updates+'"')) `
            -RedirectStandardOutput (Join-Path $serverRoot 'server.stdout.log') `
            -RedirectStandardError (Join-Path $serverRoot 'server.stderr.log')
        $process.Id | Set-Content -LiteralPath $pidFile
        & $binary status --connect "127.0.0.1:$Port" --server-name iw4l-prod --ca-cert $ca
        if ($LASTEXITCODE -ne 0) { throw "Relay startup failed; inspect $serverRoot/server.stderr.log" }
        Write-Output "Relay running on UDP $Port (PID $($process.Id))."
    }
}
