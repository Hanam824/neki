const pc = require("picocolors");
const esbuild = require("esbuild");

// esbuild runs a native binary from the package for this platform.
const { code } = esbuild.transformSync("const answer: number = 42", { loader: "ts" });
console.log(pc.green(`hello from Node ${process.version}, built by Neki`));
console.log(`esbuild ${esbuild.version} compiled TypeScript: ${code.trim()}`);
