import { Injectable } from "./di";

export interface Item {
  sku: string;
  price: number;
}

@Injectable()
export class Cart {
  private items: Item[] = [];

  add(item: Item): void {
    console.log("adding", item.sku);
    this.items.push(item);
  }

  total(): number {
    let sum = 0;
    for (const i of this.items) {
      sum += i.price;
    }
    console.log(sum);
    return sum;
  }

  async checkout(): Promise<void> {
    const res = await fetch("/api/checkout", { method: "POST" });
    if (res.status == 200) {
      debugger;
    }
    // ast-grep-ignore
    console.log("done");
  }
}

export function find(items: Item[], sku: string): Item | undefined {
  return items.find((i) => i.sku == sku);
}

export const discount = (price: number, pct: any) => price - price * pct;

var legacy = 1;
let modern = 2;
const fixed = 3;
