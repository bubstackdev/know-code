using System;

namespace Example
{
    public static class Invocations
    {
        public static int InvocationTarget(int value) => value;
        public static string InvocationTarget(string value) => value;
        public static T InvocationGeneric<T>(T value) => value;

        public static void Run(InvocationReceiver? receiver)
        {
            _ = InvocationTarget(1);
            _ = InvocationTarget("text");
            _ = InvocationGeneric(1);
            _ = 1.InvocationExtension();
            _ = "text".InvocationExtension();
            _ = receiver?.InvocationMember();
            int InvocationLocal(int value) => value;
            _ = InvocationLocal(1);
            Func<int, int> invocationDelegate = InvocationTarget;
            _ = invocationDelegate(1);
            _ = receiver?.InvocationCallback(1);
            _ = receiver?.InvocationField(1);
        }

        public static T InvocationExtension<T>(this T value) => value;
    }

    public sealed class InvocationReceiver
    {
        public int InvocationMember() => 1;
        public Func<int, int> InvocationCallback => value => value;
        public Func<int, int> InvocationField = value => value;
    }
}
