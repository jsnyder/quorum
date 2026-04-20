// Fixture: unsafe-url-concat
declare const baseUrl: string;
declare const obj: { url: string };

// match: identifier + '/path'
const e1 = baseUrl + "/v1/chat";

// match: member expression + '/segment'
const e2 = obj.url + "/segment";

// no-match: literal + literal (no variable)
const e3 = "http://a.example" + "/path";

// no-match: not a URL-like trailing literal
const e4 = baseUrl + "suffix";
