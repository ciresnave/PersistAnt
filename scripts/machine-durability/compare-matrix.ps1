# SPDX-License-Identifier: MIT OR Apache-2.0
# Compare the rows a machine-durability run generated with the checked-in matrix/opendal-machine.tsv
# (ROADMAP row 4). Comment lines and the header are ignored. Rows are recorded per OpenDAL version
# (column 4) and older versions stay: only the rows of the versions THIS run measured are compared.
#
#   no checked-in row for a measured version -> a notice, exit 0 (nothing is claimed yet: add the
#                                               generated file's rows in a PR, from the run artifact)
#   the rows differ                          -> exit 1 (a backend that held no longer does, or the
#                                               record is stale): that is the point of the test
#   the rows agree                           -> exit 0
param(
    [Parameter(Mandatory)][string]$Generated,
    [Parameter(Mandatory)][string]$Checked
)
$ErrorActionPreference = 'Stop'

function Get-Rows([string]$Path) {
    if (-not (Test-Path $Path)) { return @() }
    @(Get-Content $Path | Where-Object { $_ -and -not $_.StartsWith('#') } | Select-Object -Skip 1)
}
function Get-Version([string]$Row) { ($Row -split "`t")[3] }

$gen = @(Get-Rows $Generated)
if ($gen.Count -eq 0) { Write-Host 'no generated rows: nothing was measured, nothing to compare'; exit 0 }
$versions = @($gen | ForEach-Object { Get-Version $_ } | Sort-Object -Unique)
$chk = @(Get-Rows $Checked)
$bad = $false
foreach ($v in $versions) {
    $g = @($gen | Where-Object { (Get-Version $_) -eq $v } | Sort-Object)
    $c = @($chk | Where-Object { (Get-Version $_) -eq $v } | Sort-Object)
    if ($c.Count -eq 0) {
        Write-Host "::notice::matrix/opendal-machine.tsv has no row for OpenDAL $v; this run measured $($g.Count) (artifact machine-durability-matrix). Add them in a PR."
    } elseif (($g -join "`n") -ne ($c -join "`n")) {
        Write-Host "::error::matrix/opendal-machine.tsv differs from this run for OpenDAL $v"
        Compare-Object $c $g | ForEach-Object { Write-Host ("  {0} {1}" -f $_.SideIndicator, $_.InputObject) }
        $bad = $true
    } else {
        Write-Host "OpenDAL ${v}: $($g.Count) row(s) agree with matrix/opendal-machine.tsv"
    }
}
if ($bad) { exit 1 }
