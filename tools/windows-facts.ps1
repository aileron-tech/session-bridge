# Collects the facts needed to finish Windows support for session-bridge:
# how Claude Code and Codex expose live sessions on this machine.
# Run it in PowerShell while one Claude Code session and one Codex session are
# open, then review windows-facts.txt before sharing it: it lists session
# records, working directories, and pipe names, but no conversation content.
$ErrorActionPreference = 'Continue'
$out = Join-Path (Get-Location) 'windows-facts.txt'
$claude = if ($env:CLAUDE_CONFIG_DIR) { $env:CLAUDE_CONFIG_DIR } else { Join-Path $env:USERPROFILE '.claude' }
$codex = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
& {
    '== versions'
    $PSVersionTable.PSVersion.ToString()
    [System.Environment]::OSVersion.VersionString
    foreach ($name in 'claude', 'codex', 'sqlite3') {
        $command = Get-Command $name -ErrorAction SilentlyContinue
        if ($command) { "$name -> $($command.Source)"; & $name --version 2>&1 } else { "$name -> not found" }
    }
    '== claude session records'
    Get-ChildItem (Join-Path $claude 'sessions') -Filter *.json -ErrorAction SilentlyContinue | ForEach-Object {
        "-- $($_.Name)"
        Get-Content $_.FullName -Raw
    }
    '== claude config directory entries'
    Get-ChildItem $claude -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name
    '== named pipes mentioning claude, codex or cc-'
    [System.IO.Directory]::GetFiles('\\.\pipe\') | Where-Object { $_ -match 'claude|codex|cc-|session' }
    '== codex home entries'
    Get-ChildItem $codex -ErrorAction SilentlyContinue | Select-Object Name, Length
    '== codex thread writer locks'
    Get-ChildItem (Join-Path $codex 'thread-writer-locks') -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name
    '== codex queue help'
    codex queue --help 2>&1
    '== claude and codex processes'
    Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'claude|codex|node' } |
        Select-Object ProcessId, ParentProcessId, Name, CommandLine | Format-List
} | Out-File -FilePath $out -Encoding utf8
"Wrote $out. Review it, then share it."
