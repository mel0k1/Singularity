using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;

namespace Robust.Shared.Network.Transport;

internal static class SgTransportNative
{
    public const uint AbiVersion = 1;

    public const uint EventConnected = 0;
    public const uint EventDisconnected = 1;
    public const uint EventReceived = 2;

    public const uint DeliveryUnreliable = 0;
    public const uint DeliveryReliableOrdered = 1;

    public const int MaxDatagram = 1200;

    private const string Library = "singularity_net_transport";

    private static IntPtr _handle;

    [StructLayout(LayoutKind.Sequential)]
    internal unsafe struct SgNetEvent
    {
        public uint Kind;
        public ulong Connection;
        public uint Length;
        public uint Reserved;
        public fixed byte Data[MaxDatagram];
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct SgNetStats
    {
        public ulong BytesSent;
        public ulong BytesReceived;
        public ulong DatagramsSent;
        public ulong DatagramsReceived;
        public ulong DatagramsDropped;
        public ulong Retransmits;
        public uint Connections;
        public uint Reserved;
    }

    public static void EnsureLoaded()
    {
        if (_handle != IntPtr.Zero)
            return;

        var candidates = new List<string>();
        var env = Environment.GetEnvironmentVariable("SINGULARITY_NET_TRANSPORT_FFI");
        if (!string.IsNullOrEmpty(env))
            candidates.Add(env);

        var searchDir = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location)
                        ?? AppContext.BaseDirectory;
        for (var level = 0; level < 8 && !string.IsNullOrEmpty(searchDir); level++)
        {
            candidates.Add(Path.Combine(searchDir, "rust", "target", "release", "libsingularity_net_transport.so"));
            candidates.Add(Path.Combine(searchDir, "rust", "target", "release", "singularity_net_transport.dll"));
            candidates.Add(Path.Combine(searchDir, "rust", "target", "release", "libsingularity_net_transport.dylib"));
            candidates.Add(Path.Combine(searchDir, "libsingularity_net_transport.so"));
            candidates.Add(Path.Combine(searchDir, "singularity_net_transport.dll"));
            candidates.Add(Path.Combine(searchDir, "libsingularity_net_transport.dylib"));
            searchDir = Path.GetDirectoryName(searchDir.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar));
        }

        candidates.Add("libsingularity_net_transport.so");
        candidates.Add("singularity_net_transport.dll");
        candidates.Add("singularity_net_transport");

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
                $"native transport library '{Library}' not found; set SINGULARITY_NET_TRANSPORT_FFI to its path");

        NativeLibrary.SetDllImportResolver(typeof(SgTransportNative).Assembly, Resolve);
    }

    private static IntPtr Resolve(string libraryName, Assembly assembly, DllImportSearchPath? searchPath)
    {
        return libraryName == Library ? _handle : IntPtr.Zero;
    }

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint sg_transport_abi_version();

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr sg_transport_bind(
        [MarshalAs(UnmanagedType.LPStr)] string addr);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void sg_transport_destroy(IntPtr transport);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern ushort sg_transport_local_port(IntPtr transport);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong sg_transport_connect(
        IntPtr transport,
        [MarshalAs(UnmanagedType.LPStr)] string addr);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern uint sg_transport_send(
        IntPtr transport,
        ulong connection,
        uint delivery,
        byte* data,
        nuint length);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern nuint sg_transport_poll(
        IntPtr transport,
        SgNetEvent* events,
        nuint capacity);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void sg_transport_close(IntPtr transport, ulong connection);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint sg_transport_remote_endpoint(
        IntPtr transport,
        ulong connection,
        [Out] byte[] endpoint,
        nuint capacity);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static unsafe extern uint sg_transport_stats(
        IntPtr transport,
        SgNetStats* stats);
}
