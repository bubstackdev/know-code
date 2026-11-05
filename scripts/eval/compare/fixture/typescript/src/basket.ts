export class Basket {
  items: string[] = [];

  weigh(): number {
    return this.items.length;
  }
}
