#Requires -Version 7.0
param(
    [string]$Player = (Join-Path $PSScriptRoot '../target/release/protogine-player.exe')
)

$ErrorActionPreference = 'Stop'
if (!$IsWindows) { throw 'This native input probe requires Windows.' }
$playerPath = (Resolve-Path -LiteralPath $Player).Path
$fixture = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'fixtures/player_input.luau') -Raw
$probeRoot = Join-Path $PSScriptRoot "../target/player-input/$([Guid]::NewGuid().ToString('N'))"
$probeRoot = [IO.Path]::GetFullPath($probeRoot)
New-Item -ItemType Directory -Path (Join-Path $probeRoot 'game') -Force | Out-Null
$probeExe = Join-Path $probeRoot 'protogine-player.exe'
Copy-Item -LiteralPath $playerPath -Destination $probeExe

if (-not ('ProtogineWindowProbe' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class ProtogineWindowProbe {
    private delegate bool EnumWindow(IntPtr window, IntPtr data);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindow callback, IntPtr data);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowTextW(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] private static extern bool PostMessageW(IntPtr window, uint message, UIntPtr key, IntPtr data);
    [DllImport("user32.dll")] private static extern uint MapVirtualKeyW(uint code, uint kind);
    public static IntPtr Find(int processId) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((window, data) => {
            uint process; GetWindowThreadProcessId(window, out process);
            var title = new StringBuilder(256); GetWindowTextW(window, title, 256);
            if (process == processId && title.ToString() == "Protogine Player") {
                found = window;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static void Key(IntPtr window, uint key, bool down) {
        long flags = 1 | ((long)MapVirtualKeyW(key, 0) << 16);
        if (key >= 0x21 && key <= 0x28) flags |= 0x01000000; // Extended arrow scan codes.
        if (!down) flags |= 0xc0000000L;
        if (!PostMessageW(window, down ? 0x100U : 0x101U, (UIntPtr)key, (IntPtr)flags))
            throw new Exception("Posting key event failed");
    }
    public static void Close(IntPtr window) {
        if (!PostMessageW(window, 0x10, UIntPtr.Zero, IntPtr.Zero))
            throw new Exception("Posting window-close failed");
    }
}
'@
}

function Read-ProbeLog([string]$Path) {
    if (Test-Path -LiteralPath $Path) { return [string](Get-Content -LiteralPath $Path -Raw) }
    return ''
}

function Wait-ProbeLine($Process, [string]$Log, [string]$Line) {
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $text = [string](Read-ProbeLog $Log)
        if (($text -split '\r?\n') -contains $Line) { return }
        if ($Process.HasExited -or $text.Contains('Game error:')) { break }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Player did not report '$Line'. Log: $Log`n$text"
}

function Start-ProbePlayer([string]$Log) {
    # Exclude caller capture/dimension settings, then restore them immediately.
    $saved = @{}
    try {
        foreach ($name in @('PLAYER_CAPTURE', 'PLAYER_CAPTURE_FRAME', 'PLAYER_WIDTH', 'PLAYER_HEIGHT')) {
            $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
        return Start-Process -FilePath $probeExe -WorkingDirectory $probeRoot -WindowStyle Hidden `
            -PassThru -RedirectStandardError $Log
    } finally {
        foreach ($name in $saved.Keys) {
            if ($null -eq $saved[$name]) {
                Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
            } else {
                [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process')
            }
        }
    }
}

foreach ($finish in @('close', 'escape', 'shutdown-fault')) {
    $source = if ($finish -eq 'shutdown-fault') {
        'return {init=function(ctx) ctx.log("ready") end, shutdown=function(ctx) ctx.log("shutdown"); error("shutdown failed") end}'
    } else { $fixture }
    Set-Content -LiteralPath (Join-Path $probeRoot 'game/main.luau') -Value $source -Encoding utf8NoBOM
    $log = Join-Path $probeRoot "$finish.txt"
    $process = Start-ProbePlayer $log
    try {
        Wait-ProbeLine $process $log 'ready'
        $window = [ProtogineWindowProbe]::Find($process.Id)
        if ($window -eq [IntPtr]::Zero) { throw "Player window not found. Log: $log" }
        $expected = [Collections.Generic.List[string]]::new()
        $expected.Add('ready')
        if ($finish -eq 'close') {
            # Independent expectation per physical key: a permutation must fail.
            foreach ($mapping in @(
                @{Key=0x26; Button='up'}, @{Key=0x28; Button='down'},
                @{Key=0x25; Button='left'}, @{Key=0x27; Button='right'},
                @{Key=0x20; Button='action'}, @{Key=0x08; Button='cancel'}
            )) {
                foreach ($down in @($true, $false)) {
                    $edge = if ($down) { 'pressed' } else { 'released' }
                    $line = "$edge $($mapping.Button)"
                    [ProtogineWindowProbe]::Key($window, $mapping.Key, $down)
                    Wait-ProbeLine $process $log $line
                    $expected.Add($line)
                }
            }
        }
        if ($finish -eq 'escape') {
            [ProtogineWindowProbe]::Key($window, 0x1b, $true)
        } else {
            [ProtogineWindowProbe]::Close($window)
        }
        if (!$process.WaitForExit(10000)) { throw "Player did not exit. Log: $log" }
        $text = [string](Read-ProbeLog $log)
        $expected.Add('shutdown')
        if ($finish -eq 'shutdown-fault') {
            if ($process.ExitCode -ne 3 -or !$text.Contains('Game error: shutdown:') -or
                @($text -split '\r?\n' | Where-Object { $_ -eq 'shutdown' }).Count -ne 1) {
                throw "Failed shutdown fault check. Log: $log`n$text"
            }
        } else {
            $actual = $text.TrimEnd() -replace '\r\n', "`n"
            if ($process.ExitCode -ne 0 -or $actual -cne ($expected -join "`n")) {
                throw "Unexpected input/shutdown sequence or exit code. Log: $log`n$text"
            }
        }
        "$finish : passed (exit $($process.ExitCode))"
    } finally {
        if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
}
"Player input/shutdown checks passed. Logs: $probeRoot"
