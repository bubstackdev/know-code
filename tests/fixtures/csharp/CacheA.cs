namespace Example
{
    public static class CacheA
    {
        public static int Read(int cachedArg)
        {
            var cachedLocal = cachedArg + cachedArg;
            return cachedLocal + cachedLocal;
        }
    }
}
