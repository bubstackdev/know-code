package fixture.stock;

import java.util.ArrayList;
import java.util.List;
import java.util.Optional;

public class Inventory {
    private final List<String> items = new ArrayList<>();

    public void add(String item) {
        if (item == null) throw new IllegalArgumentException("item");
        System.out.println("adding " + item);
        items.add(item);
    }

    public String first() {
        Optional<String> head = items.stream().findFirst();
        return head.get();
    }

    public int count() {
        try {
            return items.size();
        } catch (Exception e) {
        }
        return 0;
    }

    public synchronized void clear() {
        // ast-grep-ignore: java-println
        System.out.println("clearing");
        items.clear();
    }
}
