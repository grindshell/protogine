# Drive the sprite sample with real Windows key events in a live Player.
#
# Capture mode deliberately steps with neutral input, so it can show the sample
# but never its response to a key. This posts actual WM_KEYDOWN/WM_KEYUP to the
# Player window instead, and reads what the game did back out of its log.
#
# The bundle it runs is the committed sample with one statement added: a log of
# the position, frame and facing at the end of update. Nothing else differs, and
# the script fails if that insertion does not apply.
param(
    [string]$Player = (Join-Path $PSScriptRoot '../target/release/protogine-player.exe'),
    [int]$HoldMilliseconds = 2500
)
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'This input probe requires Windows.' }

$probeRepo = Split-Path -Parent $PSScriptRoot
$probeExecutablePath = (Resolve-Path -LiteralPath $Player).Path
$probeRoot = [IO.Path]::GetFullPath((Join-Path $probeRepo "target/sprites-probe/$([Guid]::NewGuid().ToString('N'))"))
New-Item -ItemType Directory -Path (Join-Path $probeRoot 'game/assets') -Force | Out-Null
$probeExecutable = Join-Path $probeRoot 'protogine-player.exe'
Copy-Item -LiteralPath $probeExecutablePath -Destination $probeExecutable
$probeSample = Join-Path $probeRepo 'examples/games/sprites'
Copy-Item -LiteralPath (Join-Path $probeSample 'room.luau') -Destination (Join-Path $probeRoot 'game/room.luau')
Get-ChildItem -LiteralPath (Join-Path $probeSample 'assets') -Filter *.png |
    ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $probeRoot 'game/assets') }

# The one added statement, and the line it is appended to.
$probeAnchor = '        steps = if dx ~= 0 or dy ~= 0 then steps + 1 else 0'
$probeLog = '        ctx.log(`probe {ticks} {x} {y} {(steps // FRAME_TICKS) % FRAMES} {facing}`)'
$probeSource = [IO.File]::ReadAllText((Join-Path $probeSample 'main.luau'))
if (-not $probeSource.Contains($probeAnchor)) { throw 'The sample no longer contains the probe anchor line.' }
$probeSource = $probeSource.Replace($probeAnchor, "$probeAnchor`n$probeLog")
[IO.File]::WriteAllText((Join-Path $probeRoot 'game/main.luau'), $probeSource, (New-Object Text.UTF8Encoding $false))

if (-not ('ProtogineSpriteProbe' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class ProtogineSpriteProbe {
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
}
'@
}

$probeLogPath = Join-Path $probeRoot 'player.txt'
function Read-ProbeText { if (Test-Path -LiteralPath $probeLogPath) { [string](Get-Content -LiteralPath $probeLogPath -Raw) } else { '' } }
function Wait-ProbeMatch([string]$Pattern, [int]$TimeoutMilliseconds = 8000) {
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        $text = Read-ProbeText
        if ($text -match $Pattern) { return $Matches }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Player never reported /$Pattern/. Log: $probeLogPath`n$text"
}
function Get-ProbeStates {
    $states = @()
    foreach ($line in ((Read-ProbeText) -split '\r?\n')) {
        if ($line -match '^probe (\d+) (-?\d+) (-?\d+) (\d) (-?\d+)$') {
            $states += [pscustomobject]@{
                Tick = [int]$Matches[1]; X = [int]$Matches[2]; Y = [int]$Matches[3]
                Frame = [int]$Matches[4]; Facing = [int]$Matches[5]
            }
        }
    }
    return $states
}

$saved = @{}
try {
    foreach ($name in @('PLAYER_CAPTURE', 'PLAYER_CAPTURE_FRAME', 'PLAYER_WIDTH', 'PLAYER_HEIGHT')) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    }
    $probeProcess = Start-Process -FilePath $probeExecutable -WorkingDirectory $probeRoot -WindowStyle Hidden `
        -PassThru -RedirectStandardError $probeLogPath
    $null = $probeProcess.Handle
} finally {
    foreach ($name in $saved.Keys) {
        if ($null -eq $saved[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    }
}

try {
    Wait-ProbeMatch 'ready assets/tiles\.png 784x352 at tick \d+' | Out-Null
    $probeWindow = [ProtogineSpriteProbe]::Find($probeProcess.Id)
    if ($probeWindow -eq [IntPtr]::Zero) { throw "Player window not found. Log: $probeLogPath" }
    $spawn = (Get-ProbeStates)[0]

    # Hold Right long enough to leave the chamber and reach the fence past it.
    [ProtogineSpriteProbe]::Key($probeWindow, 0x27, $true)
    Start-Sleep -Milliseconds $HoldMilliseconds
    [ProtogineSpriteProbe]::Key($probeWindow, 0x27, $false)
    Start-Sleep -Milliseconds 300
    $moving = Get-ProbeStates
    $rightmost = ($moving | Measure-Object -Property X -Maximum).Maximum
    if ($rightmost -le $spawn.X) { throw "Holding Right moved nothing: still at $($spawn.X)" }
    if ($rightmost -ne 640) { throw "Expected the fence to stop the character at x=640, reached $rightmost" }
    if (($moving | Where-Object { $_.Frame -eq 1 }).Count -eq 0) { throw 'The walk never reached its second frame' }
    if ($moving[-1].Frame -ne 0) { throw 'Releasing the key should return the character to its first frame' }
    if ($moving[-1].Y -ne $spawn.Y) { throw 'Holding Right must not change the row' }

    # Face the other way: one Left tick is enough to flip the sprite.
    [ProtogineSpriteProbe]::Key($probeWindow, 0x25, $true)
    Start-Sleep -Milliseconds 200
    [ProtogineSpriteProbe]::Key($probeWindow, 0x25, $false)
    Start-Sleep -Milliseconds 200
    $facing = (Get-ProbeStates)[-1]
    if ($facing.Facing -ne -1) { throw 'Holding Left should turn the character around' }
    if ($facing.X -ge $rightmost) { throw 'Holding Left should move the character back' }

    # Space evicts both images, and the next tick asks for them again: the live
    # loading state, reached without restarting the Player.
    $before = (Get-ProbeStates)[-1].Tick
    [ProtogineSpriteProbe]::Key($probeWindow, 0x20, $true)
    Start-Sleep -Milliseconds 100
    [ProtogineSpriteProbe]::Key($probeWindow, 0x20, $false)
    $unloaded = (Wait-ProbeMatch 'unloaded both images at tick (\d+)')[1]
    if ([int]$unloaded -lt $before) { throw 'The eviction log predates the key press' }
    Wait-ProbeMatch "requested assets/character\.png at tick $([int]$unloaded + 1)" | Out-Null
    Wait-ProbeMatch 'ready assets/tiles\.png 784x352 at tick \d+[\s\S]*ready assets/tiles\.png 784x352 at tick \d+' | Out-Null

    [ProtogineSpriteProbe]::Key($probeWindow, 0x1b, $true)
    if (-not $probeProcess.WaitForExit(10000)) { throw "Player did not exit. Log: $probeLogPath" }
    $probeProcess.Refresh()
    if ($probeProcess.ExitCode -ne 0) { throw "Player exited $($probeProcess.ExitCode). Log: $probeLogPath" }
    $final = Get-ProbeStates
    "moved {0} -> {1} over {2} ticks, turned to face {3}, reloaded at tick {4}" -f `
        $spawn.X, $rightmost, $final[-1].Tick, $facing.Facing, $unloaded
    "Sprite sample input/reload checks passed. Log: $probeLogPath"
} finally {
    if (-not $probeProcess.HasExited) { $probeProcess.Kill(); $probeProcess.WaitForExit() }
    $probeProcess.Dispose()
}
