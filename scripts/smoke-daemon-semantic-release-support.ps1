# Host-level helpers for the Windows Semantic daemon smoke.

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class CtxWindowsNativeArchitecture
{
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool IsWow64Process2(
        IntPtr process,
        out ushort processMachine,
        out ushort nativeMachine);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    public static string Probe()
    {
        try
        {
            ushort processMachine;
            ushort nativeMachine;
            if (!IsWow64Process2(GetCurrentProcess(), out processMachine, out nativeMachine))
            {
                return "error";
            }
            return processMachine.ToString("X4") + ":" + nativeMachine.ToString("X4");
        }
        catch (EntryPointNotFoundException)
        {
            return "unavailable";
        }
    }
}
"@

function New-UniqueRunRoot {
    param([string]$Parent)

    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        $candidate = Join-Path $Parent ("ctx-semantic-smoke-" + [System.Guid]::NewGuid().ToString("n"))
        try {
            return (New-Item -ItemType Directory -Path $candidate -ErrorAction Stop).FullName
        } catch {
            if (Test-Path -LiteralPath $candidate) {
                continue
            }
            throw
        }
    }
    throw "Could not create a unique semantic smoke run root under $Parent"
}
