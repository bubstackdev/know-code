import { Store } from "./store";

export function Badge(props: { count: number }) {
  console.log("render badge");
  return <span className="badge">{props.count}</span>;
}

export function List(props: { items: string[] }) {
  return (
    <ul>
      {props.items.map((i) => (
        <li>{i}</li>
      ))}
    </ul>
  );
}

export const store = new Store<number>(0);
