export class Store<T> {
  private state: T;
  private listeners: Array<(s: T) => void> = [];

  constructor(initial: T) {
    this.state = initial;
  }

  subscribe(fn: (s: T) => void): () => void {
    this.listeners.push(fn);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== fn);
    };
  }

  set(next: T): void {
    this.state = next;
    this.listeners.forEach((l) => l(this.state));
    setTimeout(() => console.warn("set"), 0);
  }

  get(): T {
    return this.state as any;
  }
}
