namespace Orchard;

public static class Harvest
{
    public static int Gather(IPicker picker)
    {
        var basket = new Basket();
        picker.Pick(basket);
        return basket.Weigh();
    }

    public static int Run()
    {
        return Gather(new HandPicker()) + Gather(new HandPicker());
    }

    public static string Report()
    {
        return $"harvested {Run()}";
    }
}
