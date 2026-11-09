namespace Example
{
    public static class CacheB
    {
        public static int Read(int cachedArg)
        {
            var cachedLocal = cachedArg + cachedArg;
            return cachedLocal + cachedLocal;
        }
    }
}
