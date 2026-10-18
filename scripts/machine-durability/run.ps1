# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  The Machine durability test (kv.durability.machine): write on a Hyper-V Gen2 Linux guest, cut the
  guest's power the moment the host has seen N acknowledgements, boot it, and check that every
  acknowledged key is there. Then fill a small disk and check that the write fails loudly.

.DESCRIPTION
  Backends (the names `machine_vm` takes):
    sqlite      SQLite WAL, synchronous=FULL           must HOLD every acknowledged key
    postgres    PostgreSQL defaults in the guest       must HOLD
    mysql       MySQL/InnoDB defaults in the guest     must HOLD
    sqlite-off  SQLite synchronous=OFF                 the KNOWN-BAD CONTROL: must LOSE keys in at
                                                       least one cycle, or the harness proves nothing

  Each cycle uses a fresh database, so keys from an earlier cycle cannot satisfy a later check. The
  host learns of an acknowledgement when the line `acked N` arrives over SSH, so what it checks is
  "every key the host had seen acknowledged"; more may have been acknowledged in the guest.

  Needs the "Hyper-V Administrators" group, the image from build-image.ps1, and the Linux
  machine_vm binary (cargo build --release --example machine_vm --features sqlite,mysql on a glibc
  no newer than the guest's, i.e. Ubuntu 24.04). It only ever stops or deletes the VM it created.

  What a pass does NOT show (stated in docs/DESIGN-DURABILITY-MACHINE.md section 4): the physical
  disk's own cache and host power loss, NTFS and Windows flush behaviour, torn sector writes, and
  remote object stores. The guest power cut discards guest memory; the host stays up.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Binary,
    [string]$StateDir = 'C:\ProgramData\persistant-machine-durability',
    [string]$WorkDir = (Join-Path $env:TEMP 'persistant-machine-durability'),
    [string[]]$Backends = @('sqlite', 'postgres', 'mysql'),
    [int]$Cycles = 5,
    [int]$ControlCycles = 4,
    [int]$AckTarget = 300,
    [string]$ResultFile = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Key = Join-Path $StateDir 'id_pa'
$Base = Join-Path $StateDir 'base.vhdx'
foreach ($p in $Binary, $Key, $Base) { if (-not (Test-Path $p)) { throw "missing $p" } }
try { $null = Get-VM -ErrorAction Stop } catch {
    throw "Hyper-V is not usable by $(whoami): add the account to 'Hyper-V Administrators' and start a new session. ($($_.Exception.Message))"
}

$VmName = "pa-durability-run-$PID"
# The guest's address. NOT its DNS name: the Default Switch keeps the name of the provisioning VM (same
# hostname, another address) and answers with that, so port 22 times out. Found from the VM's own
# adapter instead (Resolve-GuestAddress); the name is the last resort.
$Guest = 'pa-durability.mshome.net'
$SshOpts = @('-i', $Key, '-o', 'StrictHostKeyChecking=no', '-o', 'UserKnownHostsFile=NUL',
    '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '-o', 'LogLevel=ERROR')
$results = [System.Collections.Generic.List[object]]::new()
$failures = [System.Collections.Generic.List[string]]::new()

function Resolve-GuestAddress {
    # 1. the address the guest reports over the Hyper-V KVP channel (needs hv_kvp_daemon in the guest);
    # 2. else the host's neighbor table entry for the VM adapter's MAC (the guest's DHCP and ARP traffic
    #    put it there). Sets $script:Guest when one is found; returns it, or $null.
    $nic = Get-VMNetworkAdapter -VMName $VmName -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $nic) { return $null }
    $ip = @($nic.IPAddresses) | Where-Object { $_ -match '^\d+\.\d+\.\d+\.\d+$' } | Select-Object -First 1
    if (-not $ip -and $nic.MacAddress) {
        $mac = ($nic.MacAddress -replace '(..)(?!$)', '$1-')
        $n = Get-NetNeighbor -AddressFamily IPv4 -ErrorAction SilentlyContinue |
            Where-Object { $_.LinkLayerAddress -eq $mac -and $_.State -notin 'Unreachable', 'Incomplete' } |
            Select-Object -First 1
        if ($n) { $ip = $n.IPAddress }
    }
    if ($ip) { $script:Guest = $ip }
    $ip
}

function Write-VmDiagnostics {
    # What a person needs to see why the guest is not reachable. Never throws.
    $ErrorActionPreference = 'Continue'
    Write-Host '--- guest unreachable: diagnostics ---'
    Get-VM -Name $VmName | Format-List Name, State, Status, Uptime, Heartbeat | Out-String | Write-Host
    $nic = Get-VMNetworkAdapter -VMName $VmName
    $nic | Format-List Name, MacAddress, SwitchName, Status, IPAddresses | Out-String | Write-Host
    Get-NetNeighbor -AddressFamily IPv4 | Where-Object { $_.State -ne 'Unreachable' } |
        Format-Table IPAddress, LinkLayerAddress, State, InterfaceAlias | Out-String | Write-Host
    Write-Host "name $Guest resolves to: $((Resolve-DnsName pa-durability.mshome.net -Type A -ErrorAction SilentlyContinue | ForEach-Object IPAddress) -join ', ')"
    Write-Host "using address: $Guest"
}

function Invoke-Guest([string]$Command) {
    # Returns @{ Code; Out } of a command run in the guest as user pa.
    # Windows PowerShell 5.1 turns native stderr into terminating errors under 'Stop'; keep it as text.
    $ErrorActionPreference = 'Continue'
    $out = & ssh @SshOpts "pa@$Guest" $Command 2>&1 | ForEach-Object { "$_" }
    @{ Code = $LASTEXITCODE; Out = ($out -join "`n") }
}

function Wait-Guest([int]$Seconds = 300) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $null = Resolve-GuestAddress
        $r = Invoke-Guest 'true'
        if ($r.Code -eq 0) { return }
        Start-Sleep -Seconds 3
    }
    Write-VmDiagnostics
    throw "the guest did not accept SSH within $Seconds s"
}

function Wait-Database([string]$Backend) {
    $probe = switch ($Backend) {
        'postgres' { 'pg_isready -q -h 127.0.0.1' }
        'mysql' { 'mysqladmin --protocol=tcp -h 127.0.0.1 -upa -ppa-test-only ping --silent' }
        default { 'true' }
    }
    $deadline = (Get-Date).AddSeconds(180)
    while ((Get-Date) -lt $deadline) {
        if ((Invoke-Guest $probe).Code -eq 0) { return }
        Start-Sleep -Seconds 2
    }
    throw "$Backend did not come up after the boot"
}

function Get-Spec([string]$Backend, [string]$Name) {
    switch ($Backend) {
        { $_ -in 'sqlite', 'sqlite-off' } { "/var/lib/pa/$Name.db" }
        'postgres' { "postgres://pa:pa-test-only@127.0.0.1/$Name" }
        'mysql' { "mysql://pa:pa-test-only@127.0.0.1/$Name" }
    }
}

function Initialize-Store([string]$Backend, [string]$Name) {
    # A fresh store, made durable before the test starts, so only the writes under test are at risk.
    $cmd = switch ($Backend) {
        { $_ -in 'sqlite', 'sqlite-off' } { "rm -f /var/lib/pa/$Name.db*; sync" }
        'postgres' { "sudo -u postgres psql -qc 'DROP DATABASE IF EXISTS $Name' -c 'CREATE DATABASE $Name' && sync" }
        'mysql' { "sudo mysql -e 'DROP DATABASE IF EXISTS $Name; CREATE DATABASE $Name;' && sync" }
    }
    if ($Backend -eq 'sqlite-off') { $cmd += '; sudo mount -o remount,commit=3600 /' }
    $r = Invoke-Guest $cmd
    if ($r.Code -ne 0) { throw "could not prepare $Backend store $Name`: $($r.Out)" }
}

function Start-PowerCut([string]$Backend, [string]$Spec, [int]$Target) {
    # Start the writer over SSH, read its acknowledgements as they arrive, and turn the guest off the
    # moment the host has seen $Target of them. Returns how many the host had seen.
    $psi = [System.Diagnostics.ProcessStartInfo]::new('ssh')
    # .NET Framework has no ArgumentList: build the string, quoting each element.
    $psi.Arguments = (($SshOpts + @("pa@$Guest", "machine_vm write $Backend '$Spec'")) | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $proc = [System.Diagnostics.Process]::Start($psi)
    $seen = 0
    try {
        while ($null -ne ($line = $proc.StandardOutput.ReadLine())) {
            if ($line -match '^acked (\d+)$') {
                $seen = [int]$Matches[1] + 1
                if ($seen -ge $Target) { break }
            }
        }
        if ($seen -lt $Target) {
            throw "the writer stopped after $seen acknowledgements: $($proc.StandardError.ReadToEnd())"
        }
        Stop-VM -Name $VmName -TurnOff -Force   # power off: guest memory is discarded, nothing is synced
    } finally {
        if (-not $proc.HasExited) { $proc.Kill() }
        $proc.Dispose()
    }
    $seen
}

function Restart-Guest {
    Start-VM -Name $VmName
    Wait-Guest
}

function Add-Result([string]$Test, [string]$Backend, [int]$Cycle, [string]$Verdict, [string]$Detail) {
    $results.Add([pscustomobject]@{ test = $Test; backend = $Backend; cycle = $Cycle; verdict = $Verdict; detail = $Detail })
    Write-Host ("{0,-9} {1,-11} cycle {2}: {3}  {4}" -f $Test, $Backend, $Cycle, $Verdict, $Detail)
}

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
$disk = Join-Path $WorkDir "$VmName.vhdx"
Copy-Item $Base $disk
try {
    $null = New-VM -Name $VmName -Generation 2 -MemoryStartupBytes 3GB -VHDPath $disk -SwitchName 'Default Switch' -Path $WorkDir
    Set-VM -Name $VmName -AutomaticCheckpointsEnabled $false -ProcessorCount 2 -StaticMemory
    Set-VMFirmware -VMName $VmName -EnableSecureBoot On -SecureBootTemplate MicrosoftUEFICertificateAuthority
    Start-VM -Name $VmName
    Wait-Guest 600

    $ErrorActionPreference = 'Continue'
    & scp @SshOpts $Binary "pa@${Guest}:/tmp/machine_vm" 2>&1 | Out-Null
    $scpCode = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($scpCode -ne 0) { throw 'could not copy machine_vm into the guest' }
    $r = Invoke-Guest 'sudo install -m 755 /tmp/machine_vm /usr/local/bin/machine_vm && sync && machine_vm 2>&1 | head -1'
    if ($r.Code -ne 0 -and $r.Out -notmatch 'usage') { throw "machine_vm does not run in the guest: $($r.Out)" }

    # --- power cuts -------------------------------------------------------------------------------
    $plan = @()
    foreach ($b in $Backends) { for ($i = 1; $i -le $Cycles; $i++) { $plan += , @($b, $i, 'hold') } }
    for ($i = 1; $i -le $ControlCycles; $i++) { $plan += , @('sqlite-off', $i, 'control') }
    $controlLost = 0
    foreach ($step in $plan) {
        $backend, $i, $role = $step
        $name = "c_$($backend.Replace('-', '_'))_$i"
        Initialize-Store $backend $name
        $spec = Get-Spec $backend $name
        $seen = Start-PowerCut $backend $spec $AckTarget
        Restart-Guest
        Wait-Database $backend
        $c = Invoke-Guest "machine_vm check $backend '$spec' $seen"
        if ($role -eq 'control') {
            if ($c.Code -eq 1) { $controlLost++; Add-Result 'powercut' $backend $i 'LOST (as the control must)' $c.Out }
            elseif ($c.Code -eq 0) { Add-Result 'powercut' $backend $i 'held (control did not lose)' $c.Out }
            else { $failures.Add("control $backend cycle ${i}: checker exit $($c.Code): $($c.Out)"); Add-Result 'powercut' $backend $i 'ERROR' $c.Out }
        } elseif ($c.Code -eq 0) {
            Add-Result 'powercut' $backend $i 'HELD' "$seen acknowledged, $($c.Out)"
        } else {
            $failures.Add("$backend cycle ${i}: acknowledged $seen, checker exit $($c.Code): $($c.Out)")
            Add-Result 'powercut' $backend $i 'FAILED' $c.Out
        }
    }
    if ($ControlCycles -gt 0 -and $controlLost -eq 0) {
        $failures.Add("the known-bad control (sqlite-off) lost nothing in $ControlCycles cycles: the harness cannot show a loss, so no backend result above counts")
    }

    # --- disk full: must fail loudly, never acknowledge and lose --------------------------------
    foreach ($b in $Backends) {
        if ($b -ne 'sqlite') { continue }   # PostgreSQL and MySQL: their data directory on the small volume is a follow-up
        $r = Invoke-Guest 'sudo pa-small make 8'
        if ($r.Code -ne 0) { throw "pa-small make: $($r.Out)" }
        $e = Invoke-Guest "machine_vm enospc $b /mnt/small/full.db"
        if ($e.Code -ne 3 -or $e.Out -notmatch 'failed (\d+) after acked (\d+)') {
            $failures.Add("enospc $b`: expected a loud failure (exit 3), got exit $($e.Code): $($e.Out)")
            Add-Result 'enospc' $b 1 'FAILED' $e.Out
        } else {
            $acked = [int]$Matches[2]
            $c = Invoke-Guest "machine_vm check $b /mnt/small/full.db $acked"
            if ($c.Code -eq 0) { Add-Result 'enospc' $b 1 'FAILED LOUD, ACKED KEPT' "$($e.Out); $($c.Out)" }
            else { $failures.Add("enospc ${b}: $acked acknowledged before the failure, but $($c.Out)"); Add-Result 'enospc' $b 1 'LOST AN ACK' $c.Out }
        }
        $null = Invoke-Guest 'sudo pa-small drop'
    }
} finally {
    if (Get-VM -Name $VmName -ErrorAction SilentlyContinue) {
        Stop-VM -Name $VmName -TurnOff -Force -ErrorAction SilentlyContinue
        Remove-VM -Name $VmName -Force
    }
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue (Join-Path $WorkDir $VmName)
    Remove-Item -Force -ErrorAction SilentlyContinue $disk
}

if ($ResultFile) { $results | ConvertTo-Json -Depth 4 | Set-Content -Path $ResultFile }
if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Host "FAIL: $_" }
    exit 1
}
Write-Host "machine durability: all $($results.Count) checks passed"
