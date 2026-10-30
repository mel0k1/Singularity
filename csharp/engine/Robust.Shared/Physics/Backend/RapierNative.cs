using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;

namespace Robust.Shared.Physics.Backend;

internal static class RapierNative
{
    public const uint AbiVersion = 1;

    public const uint OpAddBody = 1;
    public const uint OpRemoveBody = 2;
    public const uint OpAddColliderBox = 3;
    public const uint OpAddColliderCircle = 4;
    public const uint OpAddColliderPoly = 5;
    public const uint OpRemoveCollider = 6;
    public const uint OpSetTransform = 7;
    public const uint OpSetVelocity = 8;
    public const uint OpSetBodyKind = 9;
    public const uint OpSetDamping = 10;
    public const uint OpSetFixedRotation = 11;
    public const uint OpSetAwake = 12;
    public const uint OpSetGravity = 13;
    public const uint OpSetForce = 14;

    public const ulong KindDynamic = 0;
    public const ulong KindStatic = 1;
    public const ulong KindKinematic = 2;

    public const uint FlagAsleep = 1;

    private const string Library = "singularity_physics_ffi";

    private static IntPtr _handle;

    [StructLayout(LayoutKind.Sequential)]
    internal struct SgCommand
    {
        public uint Op;
        public uint Reserved;
        public ulong Id;
        public ulong Id2;
        public ulong Id3;
        public float P0;
        public float P1;
        public float P2;
        public float P3;
        public float P4;
        public float P5;
        public float P6;
        public float P7;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct SgBodyState
    {
        public ulong Id;
        public uint Kind;
        public uint Flags;
        public float X;
        public float Y;
        public float Angle;
        public float Vx;
        public float Vy;
        public float Omega;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct SgContactEvent
    {
        public ulong A;
        public ulong B;
        public uint Started;
        public uint PointCount;
        public float Nx;
        public float Ny;
        public float P0x;
        public float P0y;
        public float P1x;
        public float P1y;
    }

    public static void EnsureLoaded()
    {
        if (_handle != IntPtr.Zero)
            return;

        var candidates = new List<string>();
        var env = Environment.GetEnvironmentVariable("SINGULARITY_PHYSICS_FFI");
        if (!string.IsNullOrEmpty(env))
            candidates.Add(env);

        var searchDir = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location)
                        ?? AppContext.BaseDirectory;
        for (var level = 0; level < 8 && !string.IsNullOrEmpty(searchDir); level++)
        {
            candidates.Add(Path.Combine(searchDir, "rust", "target", "release", "libsingularity_physics_ffi.so"));
            candidates.Add(Path.Combine(searchDir, "rust", "target", "release", "singularity_physics_ffi.dll"));
            candidates.Add(Path.Combine(searchDir, "rust", "target", "release", "libsingularity_physics_ffi.dylib"));
            candidates.Add(Path.Combine(searchDir, "libsingularity_physics_ffi.so"));
            candidates.Add(Path.Combine(searchDir, "singularity_physics_ffi.dll"));
            candidates.Add(Path.Combine(searchDir, "libsingularity_physics_ffi.dylib"));
            searchDir = Path.GetDirectoryName(searchDir.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar));
        }

        candidates.Add("libsingularity_physics_ffi.so");
        candidates.Add("singularity_physics_ffi.dll");
        candidates.Add("singularity_physics_ffi");

        foreach (var candidate in candidates)
        {
            if (NativeLibrary.TryLoad(candidate, out var handle))
            {
                _handle = handle;
                break;
            }
        }

        if (_handle == IntPtr.Zero)
            throw new InvalidOperationException(
                $"native physics library '{Library}' not found; set SINGULARITY_PHYSICS_FFI to its path");

        NativeLibrary.SetDllImportResolver(typeof(RapierNative).Assembly, Resolve);
    }

    private static IntPtr Resolve(string libraryName, Assembly assembly, DllImportSearchPath? searchPath)
    {
        return libraryName == Library ? _handle : IntPtr.Zero;
    }

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint sg_abi_version();

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr sg_world_create();

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void sg_world_destroy(IntPtr world);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern uint sg_world_set_vertices(IntPtr world, float* data, UIntPtr count);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern uint sg_world_apply(IntPtr world, SgCommand* commands, UIntPtr count);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void sg_world_step(IntPtr world, float dt);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern UIntPtr sg_world_state_count(IntPtr world);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern UIntPtr sg_world_copy_states(IntPtr world, SgBodyState* states, UIntPtr capacity);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern UIntPtr sg_world_event_count(IntPtr world);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern UIntPtr sg_world_copy_events(IntPtr world, SgContactEvent* events, UIntPtr capacity);
}
