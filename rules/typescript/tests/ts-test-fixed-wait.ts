it('should do something', async () => {
    // TP: should match
    await new Promise(resolve => setTimeout(resolve, 100)); // ruleid: ts-test-fixed-wait
});

test('another test', async () => {
    // TP: should match
    setTimeout(() => {}, 500); // ruleid: ts-test-fixed-wait
});

describe('suite', () => {
    beforeEach(() => {
        // TP: should match
        setTimeout(() => {}, 10); // ruleid: ts-test-fixed-wait
    });
});

// FP: should NOT match
function helper() {
    setTimeout(() => {}, 1000); // ok: ts-test-fixed-wait
}
