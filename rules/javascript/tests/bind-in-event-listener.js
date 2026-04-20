// Fixture: bind-in-event-listener (covers both add and remove variants)

// match: bind() inside addEventListener
element.addEventListener("click", this.handler.bind(this));

// match: bind() inside removeEventListener
element.removeEventListener("click", this.handler.bind(this));

// no-match: plain callback reference
element.addEventListener("click", this.handler);

// no-match: arrow wrapper
element.addEventListener("click", () => this.handler());

// no-match: bound outside listener call
const bound = handler.bind(this);
element.addEventListener("click", bound);
