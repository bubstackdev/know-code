namespace Orchard;

public class Basket
{
    public List<string> Items { get; } = new();

    public int Weigh()
    {
        return Items.Count;
    }
}
