package fixture.stock;

public class Report {
    public static void main(String[] args) throws Exception {
        Inventory inv = new Inventory();
        inv.add("bolt");
        System.out.println(inv.count());
        Thread.sleep(10);
        String s = (String) (Object) "x";
        if (s.equals("x")) {
            System.err.println("x");
        }
    }
}
