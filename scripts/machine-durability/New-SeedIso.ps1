# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Write the files of -SourceDir into an ISO 9660 + Joliet image labelled -Label, with the IMAPI2
  COM API that ships with Windows (no mkisofs/oscdimg to install).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SourceDir,
    [Parameter(Mandatory)][string]$IsoPath,
    [string]$Label = 'cidata'
)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices.ComTypes;
public static class PaIsoWriter {
    public static void Write(object comStream, string path, int blockSize, int blocks) {
        IStream s = (IStream)comStream;
        using (FileStream f = File.Create(path)) {
            byte[] buf = new byte[blockSize * 64];
            long remaining = (long)blockSize * blocks;
            while (remaining > 0) {
                int want = (int)Math.Min(buf.Length, remaining);
                IntPtr got = System.Runtime.InteropServices.Marshal.AllocHGlobal(4);
                try {
                    s.Read(buf, want, got);
                    int n = System.Runtime.InteropServices.Marshal.ReadInt32(got);
                    if (n == 0) break;
                    f.Write(buf, 0, n);
                    remaining -= n;
                } finally { System.Runtime.InteropServices.Marshal.FreeHGlobal(got); }
            }
        }
    }
}
'@

$image = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
$image.VolumeName = $Label
$image.FileSystemsToCreate = 3   # ISO 9660 + Joliet
$image.Root.AddTree($SourceDir, $false)
$result = $image.CreateResultImage()
[PaIsoWriter]::Write($result.ImageStream, $IsoPath, $result.BlockSize, $result.TotalBlocks)
if (-not (Test-Path $IsoPath) -or (Get-Item $IsoPath).Length -eq 0) { throw "no ISO written to $IsoPath" }
Write-Host "wrote $IsoPath ($((Get-Item $IsoPath).Length) bytes, label $Label)"
