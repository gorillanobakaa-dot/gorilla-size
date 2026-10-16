# mft-check.ps1 - run ELEVATED: does --mft (the NTFS master file table) agree with the normal walk?
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File mft-check.ps1 -Exe <gorilla-size.exe> -Log <file> <folder> ...
#
# For every folder: total bytes and files from the walk and from the MFT, side by side, and the time each took.
# Read-only, like the program. The log ends with "exit <code>": 0 all agree, 1 a difference, 2 could not run.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Log,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Folders
)
$ErrorActionPreference = 'Stop'
function Say([string]$s) { Add-Content -Path $Log -Value $s -Encoding utf8; Write-Host $s }
Set-Content -Path $Log -Value "mft-check $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')" -Encoding utf8
$code = 0
try {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Say 'NOT ELEVATED: run this from an administrator prompt'; Say 'exit 2'; exit 2
    }
    Say (& $Exe --version)
    foreach ($f in $Folders) {
        $w = & $Exe $f --links --json | ConvertFrom-Json
        $m = & $Exe $f --mft --json | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0) { Say "MFT FAILED on $f"; $code = 2; continue }
        $wt = $w.roots[0].totals; $mt = $m.roots[0].totals
        $same = ($wt.bytes -eq $mt.bytes) -and ($wt.files -eq $mt.files)
        if (-not $same) { $code = [Math]::Max($code, 1) }
        Say ("{0}  {1}" -f $(if ($same) { 'AGREE ' } else { 'DIFFER' }), $f)
        Say ("        walk  {0,18:N0} bytes {1,10:N0} files  {2,7:N2} s" -f $wt.bytes, $wt.files, $w.roots[0].seconds)
        Say ("        mft   {0,18:N0} bytes {1,10:N0} files  {2,7:N2} s" -f $mt.bytes, $mt.files, $m.roots[0].seconds)
    }
} catch {
    Say "ERROR $($_.Exception.Message)"; $code = 2
}
Say "exit $code"
exit $code
