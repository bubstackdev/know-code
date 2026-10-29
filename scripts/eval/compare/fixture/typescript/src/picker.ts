import { Basket } from './basket';

export interface Picker {
  pick(basket: Basket): void;
}

export class HandPicker implements Picker {
  pick(basket: Basket): void {
    basket.items.push('pear');
  }
}
