param(
    [switch]$Once,
    [ValidateRange(20, 5000)]
    [int]$IntervalMs = 100
)

$ErrorActionPreference = "Stop"

if (-not ("XInputProbe.Native" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

namespace XInputProbe
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Gamepad
    {
        public ushort Buttons;
        public byte LeftTrigger;
        public byte RightTrigger;
        public short LeftX;
        public short LeftY;
        public short RightX;
        public short RightY;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct State
    {
        public uint PacketNumber;
        public Gamepad Gamepad;
    }

    public static class Native
    {
        [DllImport("xinput1_4.dll", CallingConvention = CallingConvention.StdCall)]
        public static extern uint XInputGetState(uint userIndex, out State state);
    }
}
"@
}

Write-Host "Reading Windows XInput slots 0-3. Press Ctrl+C to stop."
$lastLine = $null

do {
    $states = @()
    foreach ($slot in 0..3) {
        $state = New-Object XInputProbe.State
        $result = [XInputProbe.Native]::XInputGetState($slot, [ref]$state)
        if ($result -eq 0) {
            $pad = $state.Gamepad
            $states += (
                "slot={0} packet={1} buttons=0x{2:X4} LT={3} RT={4} " +
                "LX={5} LY={6} RX={7} RY={8}" -f
                $slot,
                $state.PacketNumber,
                $pad.Buttons,
                $pad.LeftTrigger,
                $pad.RightTrigger,
                $pad.LeftX,
                $pad.LeftY,
                $pad.RightX,
                $pad.RightY
            )
        }
    }

    $line = if ($states.Count -eq 0) {
        "NO_XINPUT_CONTROLLER"
    } else {
        $states -join " | "
    }
    if ($line -ne $lastLine) {
        Write-Output $line
        $lastLine = $line
    }

    if (-not $Once) {
        Start-Sleep -Milliseconds $IntervalMs
    }
} while (-not $Once)
