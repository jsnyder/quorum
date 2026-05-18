// Fixture: console-log-non-test

// match: top-level console.log in production code
console.log("debug leftover");

// match: console.debug in a class method
class Service {
  init() {
    console.debug("starting up");
  }
}

// match: console.warn in utility function
function processData(data: any) {
  console.warn("unexpected format");
  return data;
}

// no-match: inside describe block
describe("MyService", () => {
  it("should work", () => {
    console.log("test output");
  });
});

// no-match: inside test() block
test("handles edge case", () => {
  console.log("checking");
});

// no-match: inside catch clause
try {
  riskyOp();
} catch (e) {
  console.log("error caught:", e);
}

// no-match: console.error is intentional
console.error("fatal");
