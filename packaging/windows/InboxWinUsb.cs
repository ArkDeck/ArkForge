// Host-side SetupAPI binding only. This helper never opens a USB transfer handle.
// The only install target is Microsoft's existing, unchanged inbox winusb.inf.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32;
using Microsoft.Win32.SafeHandles;

[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.System32)]

namespace ArkForge.WindowsSetup
{
    public sealed class BindingSnapshot
    {
        public string InstanceId;
        public string[] HardwareIds;
        public string[] CompatibleIds;
        public string[] LocationPaths;
        public string[] UpperFilters;
        public string[] LowerFilters;
        public string Service;
        public string ClassGuid;
        public string DriverKey;
        public string Inf;
        public string Section;
        public string Version;
        public string[] InterfaceGuids;
        public bool InterfaceGuidsPresent;
        public uint Status;
        public uint Problem;
    }

    public sealed class InboxDriver
    {
        public string Inf;
        public string Section;
        public string HardwareId;
        public string Provider;
        public ulong Version;
    }

    public static class InboxWinUsb
    {
        public const string HardwareId = @"USB\VID_2207&PID_350A";
        public const string InterfaceGuid = "{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}";
        const uint ClassDriver = 1;
        static readonly IntPtr Invalid = new IntPtr(-1);
        public static string InfPath { get { return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "INF", "winusb.inf"); } }

        [StructLayout(LayoutKind.Sequential)]
        struct Device { public uint Size; public Guid Class; public uint DevInst; public UIntPtr Reserved; }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct InstallParams
        {
            public uint Size, Flags, FlagsEx;
            public IntPtr Parent, Callback, Context, Queue;
            public UIntPtr ClassReserved;
            public uint Reserved;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string Path;
        }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct Driver
        {
            public uint Size, Type; public UIntPtr Reserved;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Description;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Manufacturer;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Provider;
            public System.Runtime.InteropServices.ComTypes.FILETIME Date;
            public ulong Version;
        }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct Detail
        {
            public uint Size; public System.Runtime.InteropServices.ComTypes.FILETIME Date;
            public uint CompatibleOffset, CompatibleLength; public UIntPtr Reserved;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Section;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string Inf;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Description;
            public ushort FirstHardwareId;
        }

        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr SetupDiGetClassDevsW(IntPtr cls, string enumerator, IntPtr parent, uint flags);
        [DllImport("setupapi.dll", SetLastError = true)] static extern IntPtr SetupDiCreateDeviceInfoList(IntPtr cls, IntPtr parent);
        [DllImport("setupapi.dll", SetLastError = true)] static extern bool SetupDiEnumDeviceInfo(IntPtr set, uint index, ref Device device);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiGetDeviceRegistryPropertyW(IntPtr set, ref Device device, uint property, out uint type, byte[] data, uint bytes, out uint required);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiGetDeviceInstanceIdW(IntPtr set, ref Device device, StringBuilder text, uint size, out uint required);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiOpenDeviceInfoW(IntPtr set, string instance, IntPtr parent, uint flags, ref Device device);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiGetDeviceInstallParamsW(IntPtr set, IntPtr device, ref InstallParams parameters);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiSetDeviceInstallParamsW(IntPtr set, IntPtr device, ref InstallParams parameters);
        [DllImport("setupapi.dll", SetLastError = true)] static extern bool SetupDiBuildDriverInfoList(IntPtr set, IntPtr device, uint type);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiEnumDriverInfoW(IntPtr set, IntPtr device, uint type, uint index, ref Driver driver);
        [DllImport("setupapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool SetupDiGetDriverInfoDetailW(IntPtr set, IntPtr device, ref Driver driver, IntPtr detail, uint size, out uint required);
        [DllImport("setupapi.dll", SetLastError = true)] static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
        [DllImport("setupapi.dll", SetLastError = true)] static extern IntPtr SetupDiOpenDevRegKey(IntPtr set, ref Device device, uint scope, uint profile, uint type, uint access);
        [DllImport("cfgmgr32.dll")] static extern uint CM_Get_DevNode_Status(out uint status, out uint problem, uint devInst, uint flags);
        [DllImport("newdev.dll", SetLastError = true)] static extern bool DiInstallDevice(IntPtr parent, IntPtr set, ref Device device, ref Driver driver, uint flags, out bool reboot);

        static Device NewDevice() { return new Device { Size = (uint)Marshal.SizeOf<Device>() }; }
        static void Check(bool ok, string operation)
        {
            if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error(), operation);
        }
        static bool Same(string a, string b) { return String.Equals(a, b, StringComparison.OrdinalIgnoreCase); }
        static string[] Property(IntPtr set, ref Device device, uint property, bool optional = false)
        {
            var bytes = new byte[16384]; uint type, length;
            if (!SetupDiGetDeviceRegistryPropertyW(set, ref device, property, out type, bytes, (uint)bytes.Length, out length))
            {
                int error = Marshal.GetLastWin32Error();
                if (optional && error == 13) return new string[0]; // ERROR_INVALID_DATA: absent property
                throw new Win32Exception(error, "Read device property");
            }
            if ((type != 1 && type != 7) || length < 2 || length > bytes.Length || length % 2 != 0 || bytes[length - 1] != 0 || bytes[length - 2] != 0)
                throw new InvalidDataException("Malformed PnP string property.");
            return Encoding.Unicode.GetString(bytes, 0, (int)length).Split(new char[] { '\0' }, StringSplitOptions.RemoveEmptyEntries);
        }
        static string One(IntPtr set, ref Device device, uint property)
        {
            var values = Property(set, ref device, property);
            if (values.Length != 1) throw new InvalidDataException("Expected one PnP value.");
            return values[0];
        }
        static RegistryKey Key(IntPtr set, ref Device device, uint type, bool writable)
        {
            IntPtr raw = SetupDiOpenDevRegKey(set, ref device, 1, 0, type, writable ? 0x2001fu : 0x20019u);
            Check(raw != Invalid, "Open device configuration key");
            var handle = new SafeRegistryHandle(raw, true);
            try { return RegistryKey.FromHandle(handle, RegistryView.Registry64); }
            catch { handle.Dispose(); throw; }
        }
        static string RegistryString(RegistryKey key, string name)
        {
            var value = key.GetValue(name, null, RegistryValueOptions.DoNotExpandEnvironmentNames) as string;
            if (String.IsNullOrEmpty(value)) throw new InvalidDataException("Missing driver metadata: " + name);
            return value;
        }
        static BindingSnapshot Read(IntPtr set, ref Device device)
        {
            var instance = new StringBuilder(4096); uint required;
            Check(SetupDiGetDeviceInstanceIdW(set, ref device, instance, (uint)instance.Capacity, out required), "Read exact device instance");
            uint status, problem;
            if (CM_Get_DevNode_Status(out status, out problem, device.DevInst, 0) != 0)
                throw new InvalidOperationException("Device is no longer present.");
            var snapshot = new BindingSnapshot {
                InstanceId = instance.ToString(), HardwareIds = Property(set, ref device, 1),
                CompatibleIds = Property(set, ref device, 2, true), LocationPaths = Property(set, ref device, 0x23),
                UpperFilters = Property(set, ref device, 0x11, true), LowerFilters = Property(set, ref device, 0x12, true),
                Service = One(set, ref device, 4), ClassGuid = device.Class.ToString("B"),
                DriverKey = One(set, ref device, 9), Status = status, Problem = problem
            };
            using (var key = Key(set, ref device, 2, false))
            {
                snapshot.Inf = RegistryString(key, "InfPath");
                snapshot.Section = RegistryString(key, "InfSection");
                snapshot.Version = RegistryString(key, "DriverVersion");
            }
            using (var key = Key(set, ref device, 1, false))
            {
                var value = key.GetValue("DeviceInterfaceGUIDs", null, RegistryValueOptions.DoNotExpandEnvironmentNames);
                snapshot.InterfaceGuidsPresent = value != null;
                if (value != null && (key.GetValueKind("DeviceInterfaceGUIDs") != RegistryValueKind.MultiString || !(value is string[])))
                    throw new InvalidDataException("Existing DeviceInterfaceGUIDs is not REG_MULTI_SZ.");
                snapshot.InterfaceGuids = value as string[] ?? new string[0];
            }
            return snapshot;
        }

        // Enumeration and local driver-list construction have no installation side effects.
        public static BindingSnapshot Inspect()
        {
            IntPtr set = SetupDiGetClassDevsW(IntPtr.Zero, "USB", IntPtr.Zero, 6); // PRESENT | ALLCLASSES
            Check(set != Invalid, "Enumerate present USB devices");
            try
            {
                BindingSnapshot result = null;
                for (uint i = 0; i < 4096; i++)
                {
                    var device = NewDevice();
                    if (!SetupDiEnumDeviceInfo(set, i, ref device))
                    {
                        int error = Marshal.GetLastWin32Error();
                        if (error != 259) throw new Win32Exception(error, "Enumerate present devices");
                        if (result == null) throw new InvalidOperationException("No present DAYU200 Loader.");
                        if (result.Problem != 0 || result.LocationPaths.Length == 0)
                            throw new InvalidOperationException("Loader has a PnP problem or no exact location.");
                        if (result.UpperFilters.Length != 0 || result.LowerFilters.Length != 0)
                            throw new InvalidOperationException("Loader has device filters; refusing an unqualified driver stack.");
                        return result;
                    }
                    if (!Property(set, ref device, 1, true).Any(id => Same(id, HardwareId))) continue;
                    if (result != null) throw new InvalidOperationException("Multiple Loader devices; refusing ambiguous binding.");
                    result = Read(set, ref device);
                }
                throw new InvalidOperationException("Device enumeration exceeded its bound.");
            }
            finally { SetupDiDestroyDeviceInfoList(set); }
        }

        sealed class Selection : IDisposable
        {
            public IntPtr Set, Data;
            public Device Device;
            public Driver Driver;
            public InboxDriver Fact;
            public void Dispose()
            {
                if (Data != IntPtr.Zero) { Marshal.FreeHGlobal(Data); Data = IntPtr.Zero; }
                if (Set != IntPtr.Zero && Set != Invalid) { SetupDiDestroyDeviceInfoList(Set); Set = IntPtr.Zero; }
            }
        }
        static Selection Select(string instance)
        {
            var selected = new Selection();
            try
            {
                selected.Set = SetupDiCreateDeviceInfoList(IntPtr.Zero, IntPtr.Zero);
                Check(selected.Set != Invalid, "Create class-independent local driver list");
                var parameters = new InstallParams { Size = (uint)Marshal.SizeOf<InstallParams>() };
                Check(SetupDiGetDeviceInstallParamsW(selected.Set, IntPtr.Zero, ref parameters), "Get driver-list parameters");
                parameters.Flags |= 0x00010000; // DI_ENUMSINGLEINF
                parameters.FlagsEx |= 0x00000800; // DI_FLAGSEX_ALLOWEXCLUDEDDRVS
                parameters.Path = InfPath;
                Check(SetupDiSetDeviceInstallParamsW(selected.Set, IntPtr.Zero, ref parameters), "Limit local search to inbox INF");
                Check(SetupDiBuildDriverInfoList(selected.Set, IntPtr.Zero, ClassDriver), "Build inbox driver list");
                selected.Device = NewDevice();
                // Documented inheritance associates the unchanged system driver list
                // with this exact instance, even when its current class is Rockusb.
                Check(SetupDiOpenDeviceInfoW(selected.Set, instance, IntPtr.Zero, 2, ref selected.Device), "Inherit inbox list for exact device");
                selected.Data = Marshal.AllocHGlobal(Marshal.SizeOf<Device>());
                Marshal.StructureToPtr(selected.Device, selected.Data, false);
                bool found = false;
                for (uint i = 0; i < 32; i++)
                {
                    var driver = new Driver { Size = (uint)Marshal.SizeOf<Driver>() };
                    if (!SetupDiEnumDriverInfoW(selected.Set, selected.Data, ClassDriver, i, ref driver))
                    {
                        int error = Marshal.GetLastWin32Error();
                        if (error != 259) throw new Win32Exception(error, "Enumerate inherited inbox drivers");
                        if (!found) throw new InvalidOperationException("System WinUSB function driver is unavailable.");
                        return selected;
                    }
                    IntPtr buffer = Marshal.AllocHGlobal(16384);
                    try
                    {
                        Marshal.WriteInt32(buffer, Marshal.SizeOf<Detail>()); uint required;
                        Check(SetupDiGetDriverInfoDetailW(selected.Set, selected.Data, ref driver, buffer, 16384, out required), "Read inherited driver details");
                        var detail = Marshal.PtrToStructure<Detail>(buffer);
                        int offset = (int)Marshal.OffsetOf<Detail>("FirstHardwareId");
                        if (required > 16384 || required < offset + 2 || (required - offset) % 2 != 0)
                            throw new InvalidDataException("Invalid driver hardware-ID buffer.");
                        var idBytes = new byte[required - offset];
                        Marshal.Copy(IntPtr.Add(buffer, offset), idBytes, 0, idBytes.Length);
                        string idList = Encoding.Unicode.GetString(idBytes);
                        int terminator = idList.IndexOf('\0');
                        if (terminator < 0) throw new InvalidDataException("Unterminated driver hardware ID.");
                        string hardwareId = idList.Substring(0, terminator);
                        if (!Same(detail.Section, "WINUSB") || !Same(hardwareId, @"USB\MS_COMP_WINUSB")) continue;
                        if (!Same(Path.GetFullPath(detail.Inf), Path.GetFullPath(InfPath)) || !Same(driver.Provider, "Microsoft") || found)
                            throw new InvalidDataException("Inbox WinUSB identity is not unique and exact.");
                        selected.Driver = driver;
                        selected.Fact = new InboxDriver { Inf = detail.Inf, Section = detail.Section, HardwareId = hardwareId, Provider = driver.Provider, Version = driver.Version };
                        found = true;
                    }
                    finally { Marshal.FreeHGlobal(buffer); }
                }
                throw new InvalidOperationException("Driver enumeration exceeded its bound.");
            }
            catch { selected.Dispose(); throw; }
        }
        public static InboxDriver InspectInbox(string instance)
        {
            using (var selected = Select(instance)) { return selected.Fact; }
        }

        // Caller must persist its one-use intent before entering this function.
        // No retry, fallback driver, automatic rollback or USB transfer is performed.
        public static bool Bind(BindingSnapshot expected)
        {
            var current = Inspect();
            AssertUnchanged(expected, current);
            using (var selected = Select(current.InstanceId))
            {
                AssertUnchanged(expected, Read(selected.Set, ref selected.Device));
                var parameters = new InstallParams { Size = (uint)Marshal.SizeOf<InstallParams>() };
                Check(SetupDiGetDeviceInstallParamsW(selected.Set, selected.Data, ref parameters), "Get exact device install parameters");
                parameters.Flags |= 0x00800000; // DI_QUIETINSTALL: OS refusal instead of a hidden wizard.
                parameters.FlagsEx |= 0x20000000; // DI_FLAGSEX_RESTART_DEVICE_ONLY
                Check(SetupDiSetDeviceInstallParamsW(selected.Set, selected.Data, ref parameters), "Constrain installation to the exact device");
                using (var key = Key(selected.Set, ref selected.Device, 1, true))
                {
                    var guids = current.InterfaceGuids.ToList();
                    if (!guids.Any(value => Same(value, InterfaceGuid))) guids.Add(InterfaceGuid);
                    // Write only this device's app interface list; never change IDs,
                    // driver signature policy, system trust or other devices.
                    key.SetValue("DeviceInterfaceGUIDs", guids.ToArray(), RegistryValueKind.MultiString);
                    key.Flush();
                }
                bool reboot;
                Check(DiInstallDevice(IntPtr.Zero, selected.Set, ref selected.Device, ref selected.Driver, 0, out reboot), "Bind exact device to system WinUSB");
                return reboot;
            }
        }

        public static void AssertUnchanged(BindingSnapshot expected, BindingSnapshot actual)
        {
            if (expected == null || actual == null || !Same(expected.InstanceId, actual.InstanceId) ||
                !Same(expected.Service, actual.Service) || !Same(expected.ClassGuid, actual.ClassGuid) ||
                !Same(expected.DriverKey, actual.DriverKey) || !Same(expected.Inf, actual.Inf) ||
                !Same(expected.Section, actual.Section) || !Same(expected.Version, actual.Version) ||
                expected.Problem != 0 || actual.Problem != 0 ||
                expected.InterfaceGuidsPresent != actual.InterfaceGuidsPresent ||
                !ArraysEqual(expected.HardwareIds, actual.HardwareIds) ||
                !ArraysEqual(expected.CompatibleIds, actual.CompatibleIds) ||
                !ArraysEqual(expected.LocationPaths, actual.LocationPaths) ||
                !ArraysEqual(expected.UpperFilters, actual.UpperFilters) ||
                !ArraysEqual(expected.LowerFilters, actual.LowerFilters) ||
                !ArraysEqual(expected.InterfaceGuids, actual.InterfaceGuids))
                throw new InvalidOperationException("Device identity or binding changed since the plan; no installation.");
        }
        static bool ArraysEqual(string[] a, string[] b)
        {
            return a != null && b != null && a.SequenceEqual(b, StringComparer.OrdinalIgnoreCase);
        }
    }
}
