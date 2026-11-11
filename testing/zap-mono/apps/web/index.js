"use strict";

const lib = require("@mono/lib");
const chalk = require("chalk");

console.log(lib.greet("workspace"), require("chalk").default.green("pnp-ok"));
