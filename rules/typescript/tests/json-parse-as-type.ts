// Fixture: json-parse-as-type
type UserData = { id: string; name: string };
declare const userInput: string;

// match: JSON.parse cast to concrete type
const u1 = JSON.parse(userInput) as UserData;

// no-match: cast to unknown (intentional widening)
const u2 = JSON.parse(userInput) as unknown;

// no-match: cast to any (caught by as-any-cast)
const u3 = JSON.parse(userInput) as any;

// no-match: parsed and validated separately
const raw = JSON.parse(userInput);
