// TP: should match - console.log left in code
console.log("debug");  // ruleid: console-log-artifact
console.log(data, "info");  // ruleid: console-log-artifact
console.log();  // ruleid: console-log-artifact

// FP: should NOT match - console.log in catch (error reporting)
try {
  doSomething();
} catch (e) {
  console.log(e);  // ok: console-log-artifact
}

// FP: console.error/warn are intentional
console.error("fatal error");  // ok: console-log-artifact
console.warn("deprecation");  // ok: console-log-artifact
