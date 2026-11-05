namespace Orchard;

public interface IPicker
{
    void Pick(Basket basket);
}

public class HandPicker : IPicker
{
    public void Pick(Basket basket)
    {
        basket.Items.Add("pear");
    }
}
