# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Build the guest disk for the Machine durability test, once, reproducibly: a pinned Ubuntu 24.04
  cloud image converted to VHDX, provisioned by cloud-init (PostgreSQL, MySQL, a test user, sysctls),
  then powered off. Output: <StateDir>\base.vhdx and <StateDir>\id_pa (the driver's SSH key).

.DESCRIPTION
  Run by a person (or the PM) on the machine that hosts the runner, as a user in the "Hyper-V
  Administrators" group with Docker available (qemu-img runs in a container; nothing is installed on
  the host). The runner account (gh-runner) needs no Docker: it only reads base.vhdx and id_pa.
  Not run by CI. Re-run to refresh the image; the pinned release and its SHA-256 below say which one.
  Needs about 4 GB of disk while building and 3-4 GB afterwards.
#>
[CmdletBinding()]
param(
    [string]$StateDir = 'C:\ProgramData\persistant-machine-durability',
    [int]$DiskGB = 10,
    [int]$TimeoutMinutes = 25,
    [string]$RunnerAccount = 'gh-runner'
)
$ErrorActionPreference = 'Stop'

# Pinned: https://cloud-images.ubuntu.com/releases/noble/release-20261017/SHA256SUMS
$Release = 'release-20261017'
$ImageName = 'ubuntu-24.04-server-cloudimg-amd64.img'
$ImageSha256 = '04e8f2461a16a44e78a370284471f29616cdd9baf1893df25807b71f985b6c94'
$ImageUrl = "https://cloud-images.ubuntu.com/releases/noble/$Release/$ImageName"

New-Item -ItemType Directory -Force -Path $StateDir | Out-Null
$dl = Join-Path $StateDir 'download'
New-Item -ItemType Directory -Force -Path $dl | Out-Null
$img = Join-Path $dl $ImageName

if (-not (Test-Path $img) -or (Get-FileHash $img -Algorithm SHA256).Hash.ToLower() -ne $ImageSha256) {
    Write-Host "downloading $ImageUrl"
    Invoke-WebRequest -Uri $ImageUrl -OutFile $img
}
$got = (Get-FileHash $img -Algorithm SHA256).Hash.ToLower()
if ($got -ne $ImageSha256) { throw "SHA-256 of $ImageName is $got, pinned ${ImageSha256}: refusing it" }
Write-Host "image verified: $ImageSha256"

# qcow2 -> dynamic VHDX, grown (qemu-img cannot resize a dynamic VHDX, so the qcow2 is resized first) to the size the databases need, in a container (no host install).
$base = Join-Path $StateDir 'base.vhdx'
Remove-Item -Force -ErrorAction SilentlyContinue $base
docker run --rm -v "${dl}:/in:ro" -v "${StateDir}:/out" ubuntu:24.04 sh -c `
    "apt-get update -qq && apt-get install -y -qq qemu-utils >/dev/null && cp /in/$ImageName /out/tmp.qcow2 && chmod 644 /out/tmp.qcow2 && qemu-img resize /out/tmp.qcow2 ${DiskGB}G && qemu-img convert -f qcow2 -O vhdx -o subformat=dynamic /out/tmp.qcow2 /out/base.vhdx && rm /out/tmp.qcow2"
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $base)) { throw 'qemu-img conversion failed' }

# The driver's key. Left in place if it exists and opens without a passphrase, so a rebuilt image keeps working for existing runners.
$key = Join-Path $StateDir 'id_pa'
# The empty passphrase goes through cmd: PowerShell 7 passes '""' to a native program as the two-character
# passphrase "" (PowerShell 5.1 passes an empty one), and a key with a passphrase cannot sign under
# BatchMode, so the guest accepted the key and then refused the login (run 39177227624).
function Test-KeyOpens([string]$Path) {
    if (-not (Test-Path $Path)) { return $false }
    cmd /c "ssh-keygen -y -P """" -f ""$Path"" >NUL 2>&1"
    $LASTEXITCODE -eq 0
}
if (-not (Test-KeyOpens $key)) {
    # A missing key, or one an earlier run made with a passphrase: make a new one (the image is rebuilt
    # below, so the guest gets the new public half).
    Remove-Item -Force -ErrorAction SilentlyContinue $key, "$key.pub"
    cmd /c "ssh-keygen -q -t ed25519 -N """" -C persistant-machine-durability -f ""$key"""
    if (-not (Test-KeyOpens $key)) { throw "$key still needs a passphrase" }
}
# The runner account only reads the image and the key. ssh-keygen gives id_pa an owner-only ACL with
# inheritance removed, so the grant on the directory never reaches it (run 39176940159: the runner
# could not even read the key). Grant it on the key itself, and read it back.
icacls $StateDir /grant "${RunnerAccount}:(OI)(CI)RX" /T | Out-Null
icacls $key /grant "${RunnerAccount}:(R)" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "could not grant $RunnerAccount read on $key" }
if (-not ((icacls $key) -match [regex]::Escape($RunnerAccount))) { throw "$RunnerAccount is not in the ACL of $key after the grant" }
$pub = (Get-Content "$key.pub" -Raw).Trim()

# Seed ISO (cloud-init NoCloud: volume label "cidata"), written with the IMAPI2 COM API: no tools to install.
$seedDir = Join-Path $StateDir 'seed'
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $seedDir
New-Item -ItemType Directory -Force -Path $seedDir | Out-Null
$userData = (Get-Content (Join-Path $PSScriptRoot 'guest-user-data.yaml') -Raw).Replace('@@PUBKEY@@', $pub)
[IO.File]::WriteAllText((Join-Path $seedDir 'user-data'), $userData.Replace("`r`n", "`n"))
[IO.File]::WriteAllText((Join-Path $seedDir 'meta-data'), "instance-id: pa-durability-image`nlocal-hostname: pa-durability`n")
$iso = Join-Path $StateDir 'seed.iso'
& (Join-Path $PSScriptRoot 'New-SeedIso.ps1') -SourceDir $seedDir -IsoPath $iso -Label cidata

# Provision: boot the disk with the seed attached, wait for cloud-init to power it off.
$vmName = 'pa-durability-image'
if (Get-VM -Name $vmName -ErrorAction SilentlyContinue) { Stop-VM -Name $vmName -TurnOff -Force -ErrorAction SilentlyContinue; Remove-VM -Name $vmName -Force }
$vm = New-VM -Name $vmName -Generation 2 -MemoryStartupBytes 2GB -VHDPath $base -SwitchName 'Default Switch' -Path $StateDir
Set-VM -Name $vmName -AutomaticCheckpointsEnabled $false -ProcessorCount 2
Set-VMFirmware -VMName $vmName -EnableSecureBoot On -SecureBootTemplate MicrosoftUEFICertificateAuthority
Add-VMDvdDrive -VMName $vmName -Path $iso
Set-VMFirmware -VMName $vmName -FirstBootDevice (Get-VMHardDiskDrive -VMName $vmName)
try {
    Start-VM -Name $vmName
    $deadline = (Get-Date).AddMinutes($TimeoutMinutes)
    while ((Get-VM -Name $vmName).State -ne 'Off') {
        if ((Get-Date) -gt $deadline) { throw "provisioning did not finish in $TimeoutMinutes minutes" }
        Start-Sleep -Seconds 10
    }
    Write-Host 'provisioned and powered off'
} finally {
    if ((Get-VM -Name $vmName).State -ne 'Off') { Stop-VM -Name $vmName -TurnOff -Force }
    Remove-VM -Name $vmName -Force
    Remove-Item -Force -ErrorAction SilentlyContinue $iso
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $seedDir
}
Write-Host "base image: $base ($([math]::Round((Get-Item $base).Length / 1MB)) MB on disk). Grant the runner account read access to $StateDir."
