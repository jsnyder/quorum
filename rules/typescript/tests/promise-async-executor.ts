// TP: should match - async executor in Promise constructor
const p = new Promise(async (resolve, reject) => {  // ruleid: promise-async-executor
  const data = await fetchData();
  resolve(data);
});

// FP: should NOT match - sync executor
const q = new Promise((resolve, reject) => {  // ok: promise-async-executor
  setTimeout(() => resolve("done"), 100);
});

// FP: should NOT match - async function not in Promise
async function getData() {  // ok: promise-async-executor
  return await fetchData();
}
