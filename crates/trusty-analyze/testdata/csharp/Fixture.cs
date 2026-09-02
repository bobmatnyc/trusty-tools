// Fixture for tests/csharp_roslyn_integration.rs (#1008).
//
// Deliberately fails to compile with CS0029 (cannot implicitly convert
// string to int) — a plain compiler error, not an optional analyzer rule, so
// the diagnostic is guaranteed regardless of which .NET SDK analyzer
// packages happen to be installed on the CI runner.
public class Fixture
{
    public static void Broken()
    {
        int mismatch = "not an int";
    }
}
