# Drive the sprite sample with real Windows key events in a live Player.
#
# Capture mode deliberately steps with neutral input, so it can show the sample
# but never its response to a key. This posts actual WM_KEYDOWN/WM_KEYUP to the
# Player window instead, and reads what the game did back out of its log.
#
# The bundle it runs is the committed sample with one statement added: a log of
# the completed tick's position, frame and facing. Nothing else differs, and the
# script fails if that insertion does not apply.
#
# **The anchor moved when the sample stopped owning its own position.** Movement
# now happens in the fixed pass after update returns, so the old log at the end
# of update would have reported the position from before this tick's movement
# while labelling it with this tick's number, and the frame and facing beside it
# would have been a tick newer than the coordinates. The log therefore sits at
# the top of update, immediately after the position is read and before `ticks`
# is incremented, where all five fields describe the end of the last completed
# tick. That is the plan's "previous completed position at the next update
# boundary, with the matching completed-tick label".
#
# The frozen field format is
#     probe <completed ticks> <x> <y> <animation frame> <facing>
# with every field a whole number. `Get-ProbeStates` enforces exactly that and
# throws on anything else it finds on a `probe` line. It used to skip what it
# could not parse, which was safe when the sample kept whole-pixel integers of
# its own and is not safe now: the engine clamps a blocked body onto a tile face
# and may leave a sub-pixel gap, so a rounding regression would print a
# fractional coordinate and the old filter would have silently read an empty or
# stale sample rather than reporting anything wrong.
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

# The one added statement, and the line it is appended to. `ticks` has not been
# incremented yet at this point and `p` is what the fixed pass committed at the
# end of that tick, so the label and the five fields agree.
$probeAnchor = '        local p = world.position(body)'
$probeLog = '        ctx.log(`probe {ticks} {p.x} {p.y} {(steps // FRAME_TICKS) % FRAMES} {facing}`)'
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
    $text = Read-ProbeText
    $lines = @($text -split '\r?\n')
    # The Player is still writing, so a read can catch the last line half
    # written. Everything before the final newline is complete, and a `probe`
    # line that survives that trim has to be readable.
    if ($lines.Count -gt 0 -and -not $text.EndsWith("`n")) {
        $lines = @($lines | Select-Object -First ($lines.Count - 1))
    }
    $states = @()
    foreach ($line in $lines) {
        if ($line -notlike 'probe *') { continue }
        if ($line -notmatch '^probe (\d+) (-?\d+) (-?\d+) (\d) (-?\d+)$') {
            throw "Unreadable probe line, so this harness cannot see the sample's state: '$line'. Log: $probeLogPath"
        }
        $states += [pscustomobject]@{
            Tick = [int]$Matches[1]; X = [int]$Matches[2]; Y = [int]$Matches[3]
            Frame = [int]$Matches[4]; Facing = [int]$Matches[5]
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
    # The stop is the engine's now: 640 is the fence tile's own face, resolved
    # by the fixed collision pass and read back out of the kernel, rather than a
    # number the script chose for itself. Every target here is one the character
    # saturates against, so holding a key longer than necessary cannot change
    # the answer.
    [ProtogineSpriteProbe]::Key($probeWindow, 0x27, $true)
    Start-Sleep -Milliseconds $HoldMilliseconds
    [ProtogineSpriteProbe]::Key($probeWindow, 0x27, $false)
    Start-Sleep -Milliseconds 300
    $moving = Get-ProbeStates
    $rightmost = ($moving | Measure-Object -Property X -Maximum).Maximum
    if ($rightmost -le $spawn.X) { throw "Holding Right moved nothing: still at $($spawn.X)" }
    if ($rightmost -ne 640) { throw "Expected the fence to stop the character at x=640, reached $rightmost" }
    # `@()`: safe here only because `$states` holds `[pscustomobject]`, which
    # has no intrinsic `Count` for PowerShell's scalar unification to shadow.
    # An element type that carries its own `Count` - a hashtable, say - would
    # have a single match report that member instead. Wrapped rather than
    # explained at each site.
    if (@($moving | Where-Object { $_.Frame -eq 1 }).Count -eq 0) { throw 'The walk never reached its second frame' }
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

    # Backspace returns to the spawn. That is a fallible `set_position` against
    # the installed map now rather than an assignment the game could not be
    # refused, so a live key event is what shows the engine accepted it.
    [ProtogineSpriteProbe]::Key($probeWindow, 0x08, $true)
    Start-Sleep -Milliseconds 100
    [ProtogineSpriteProbe]::Key($probeWindow, 0x08, $false)
    Start-Sleep -Milliseconds 300
    $returned = (Get-ProbeStates)[-1]
    if ($returned.X -ne $spawn.X -or $returned.Y -ne $spawn.Y) {
        throw "Backspace should teleport to $($spawn.X),$($spawn.Y), not $($returned.X),$($returned.Y)"
    }
    if ($returned.Facing -ne 1) { throw 'Returning to the spawn should reset facing' }

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
    "moved {0} -> {1} over {2} ticks, turned to face {3}, teleported home to {4},{5}, reloaded at tick {6}" -f `
        $spawn.X, $rightmost, $final[-1].Tick, $facing.Facing, $returned.X, $returned.Y, $unloaded
    "Sprite sample input/teleport/reload checks passed. Log: $probeLogPath"
} finally {
    if (-not $probeProcess.HasExited) { $probeProcess.Kill(); $probeProcess.WaitForExit() }
    $probeProcess.Dispose()
}
