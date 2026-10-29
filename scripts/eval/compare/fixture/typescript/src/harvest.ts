import { Basket } from './basket';
import { Picker, HandPicker } from './picker';

export function gather(picker: Picker): number {
  const basket = new Basket();
  picker.pick(basket);
  return basket.weigh();
}

export function harvest(): number {
  return gather(new HandPicker()) + gather(new HandPicker());
}

export function report(): string {
  return `harvested ${harvest()}`;
}
