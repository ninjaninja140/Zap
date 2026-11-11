import { greet } from "@mono/lib";
import chalk from "chalk";

const msg: string = greet("typescript");
console.log(msg, chalk.green("ts-ok"));

const ok: string = greet("nope");
console.log(ok);
