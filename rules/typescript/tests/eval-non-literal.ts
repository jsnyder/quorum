// Fixture: eval-non-literal
declare const userCode: string;

// match: eval on identifier
eval(userCode);

// match: eval on template with interpolation
eval(`return ${userCode}`);

// match: new Function() with identifier body
new Function(userCode);

// no-match: eval on plain string literal
eval("1 + 1");

// no-match: eval on literal template (no substitution)
eval(`return 42`);

// no-match: new Function with literal body
new Function("return 42");
