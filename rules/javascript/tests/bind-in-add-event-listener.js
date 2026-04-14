// TP: should match - bind() in addEventListener
element.addEventListener("click", this.handler.bind(this));  // ruleid: bind-in-add-event-listener
window.addEventListener("resize", onResize.bind(this));  // ruleid: bind-in-add-event-listener

// FP: should NOT match - no bind
element.addEventListener("click", this.handler);  // ok: bind-in-add-event-listener
element.addEventListener("click", () => this.handler());  // ok: bind-in-add-event-listener

// FP: should NOT match - bind not inside addEventListener
const bound = handler.bind(this);  // ok: bind-in-add-event-listener
element.addEventListener("click", bound);  // ok: bind-in-add-event-listener
