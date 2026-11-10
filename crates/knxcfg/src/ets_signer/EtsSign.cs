// Signs project directories with ETS's own signing code: knxcfg runs this
// under Mono (Linux) or .NET (Windows) as
//
//   EtsSign.exe <Knx.Ets.XmlSigning.dll> <directory>...
//
// For each directory it calls the assembly's internal
// XmlSigning.SignDirectory(path, false, { ".chw" }) by reflection and prints
// "<directory>\t<signature file>". The assembly's references are loaded from
// its own folder.
using System;
using System.IO;
using System.Reflection;

class EtsSign
{
    static int Main(string[] args)
    {
        if (args.Length < 2) {
            Console.Error.WriteLine("usage: EtsSign.exe <Knx.Ets.XmlSigning.dll> <directory>...");
            return 2;
        }
        string dll = Path.GetFullPath(args[0]);
        string dir = Path.GetDirectoryName(dll);
        AppDomain.CurrentDomain.AssemblyResolve += (sender, e) => {
            string candidate = Path.Combine(dir, new AssemblyName(e.Name).Name + ".dll");
            return File.Exists(candidate) ? Assembly.LoadFrom(candidate) : null;
        };
        try {
            Type type = Assembly.LoadFrom(dll).GetType("Knx.Ets.XmlSigning.XmlSigning", true);
            MethodInfo sign = type.GetMethod("SignDirectory", BindingFlags.Static | BindingFlags.NonPublic | BindingFlags.Public,
                null, new[] { typeof(string), typeof(bool), typeof(string[]) }, null);
            if (sign == null) {
                Console.Error.WriteLine("XmlSigning.SignDirectory(string, bool, string[]) not found in " + dll);
                return 1;
            }
            for (int i = 1; i < args.Length; i++) {
                // TrimEnd(char[]): .NET Framework has no TrimEnd(char).
                string project = Path.GetFullPath(args[i]).TrimEnd(new[] { '\\', '/' });
                sign.Invoke(null, new object[] { project, false, new[] { ".chw" } });
                string name = Path.GetFileName(project) + ".signature";
                string inside = Path.Combine(project, name);
                string beside = Path.Combine(Path.GetDirectoryName(project), name);
                Console.WriteLine(project + "\t" + (File.Exists(inside) ? inside : beside));
            }
            return 0;
        } catch (TargetInvocationException e) {
            Console.Error.WriteLine(e.InnerException);
            return 1;
        } catch (Exception e) {
            Console.Error.WriteLine(e);
            return 1;
        }
    }
}
