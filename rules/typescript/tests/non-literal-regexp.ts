// Test fixture: non-literal-regexp rule

// match: variable as pattern source
const re1 = new RegExp(userInput);

// match: variable with flags
const re2 = new RegExp(pattern, "g");

// match: function call result as pattern
const re3 = new RegExp(getPattern());

// match: template literal (dynamic)
const re4 = new RegExp(`${prefix}.*`);

// no-match: string literal pattern
const safe1 = new RegExp("^[a-z]+$");

// no-match: single-quoted string literal
const safe2 = new RegExp('fixed-pattern');

// no-match: literal regex (not RegExp constructor)
const safe3 = /^[a-z]+$/;
