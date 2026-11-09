import { Index } from './index.mjs';
console.log(JSON.stringify(await new Index(process.argv[2]).query(JSON.parse(process.argv[3]))));
