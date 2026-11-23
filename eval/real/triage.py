"""Keep/drop judgments for the 95 candidates in all.jsonl, by line index.

Each kept entry: (what the fix did, the defect, category, needs_other_files,
difficulty). Judged once against the comment and its fix hunk; indices are only
valid for the all.jsonl that mine.py rebuilds with the --until values in
mine_all.sh. Writes triaged.jsonl.
"""
import json,collections
from pathlib import Path
HERE=Path(__file__).parent
K={
1:("Fix selects conditional branch before unary op","Unary minus/bitwise-not on a conditional type resolving to bigint is inferred as number.","correctness",False,"medium"),
2:("Fix stops skipping conditional types in object-rest arm","Object rest destructuring of a conditional-typed value yields an empty object type because the conditional is skipped.","correctness",True,"hard"),
3:("Fix stops discarding conditional members when indexing","Indexing a tuple union drops members that are conditional types, giving too narrow an element type.","correctness",True,"hard"),
4:("Fix stops skipping conditional in nullish scan","Optional chaining on a union with a conditional null member omits undefined from the result type.","correctness",True,"hard"),
5:("Import of nonexistent generated IDs removed","Imports three global type IDs that do not exist in the generated ids module, breaking compilation.","build",True,"medium"),
8:("Fix explicitly rejects TwValueThemeReference","Tailwind theme-reference custom properties have no identifier text, so is_none_or accepts them and forces compound-list layout.","correctness",True,"medium"),
17:("Fix marks contributions incomplete when integration dropped","Integrations dropped at max nesting depth are not recorded as incomplete, so consumers emit false positives.","correctness",True,"medium"),
19:("Fix propagates nested comma to parent alternation","For {{js,ts}} the outer braces are escaped because only the inner pair records a comma, so the glob matches wrong names.","correctness",False,"medium"),
21:("Fix classifies legacy SCSS if() as dynamic","Legacy SCSS if() is treated as static, so the shorthand-merge fix can change semantics when a branch yields null.","correctness",False,"hard"),
22:("Fix classifies CSS if() as dynamic","CSS if() is treated as static though it can expand to multiple values, so the split autofix changes meaning.","correctness",False,"medium"),
23:("Fix sets slot_owner for v-for default lookups","A v-for default value resolves names against the same element's v-slot parameters, which are not in scope there.","correctness",False,"hard"),
29:("Fix continues operator walk through type assertions","Parenthesis check stops at a type assertion, so the formatter drops required parens around an instantiation expression before +.","correctness",False,"hard"),
31:("Fix wraps each listener call in try/catch","A throwing plugin-error listener aborts the loop and propagates, skipping reload notification and provider cleanup.","correctness",True,"medium"),
32:("Fix passes currentModelOutput to each processor","Each processor receives the original model-output mapping instead of the previous processor's committed one, undoing earlier edits.","data-integrity",False,"medium"),
33:("Fix adds snapshot comparison besides identity","Result change detection uses object identity, so a processor that mutates the result in place leaves the stale mapping in use.","data-integrity",True,"hard"),
36:("Fix tracks held ownership and releases on failure","A failure between acquiring run ownership and starting execution never releases the lease, leaving renewal running.","resource",False,"medium"),
38:("Fix uses nextGreenRun with repository identity","The next green run is matched only by workflow and branch name, so a fork run with the same branch name can be selected.","correctness",False,"medium"),
39:("Fix filters by repo identity not branch name","Filtering out head_branch main drops every run of PRs opened from a fork's main branch.","correctness",False,"easy"),
41:("Fix reports unknown status instead of none connected","Summary says none connected even when an integration check failed and its status is unknown.","correctness",False,"easy"),
45:("Fix releases lease when snapshot read rejects","If loading the workflow snapshot throws, the acquired resume lease is never released, blocking recovery until expiry.","resource",False,"easy"),
46:("Fix trims and rejects empty project name","A blank or whitespace --name bypasses validation and creates a whitespace directory or targets the current directory.","correctness",False,"easy"),
48:("Fix returns settled:false for head without card","A merge-request head update with no card yet is reported settled, so discovery never retries recording that head.","data-integrity",True,"hard"),
51:("Fix removes page cap for follow-up discovery","Each sweep restarts at page one with a five-page cap, so follow-ups beyond page five are never ingested.","correctness",True,"hard"),
59:("Fix adds nested option to claim child lease","Standalone restart of a nested child run gets a no-op lease, so it can overlap a concurrent resume of the same run.","concurrency",False,"hard"),
60:("Fix takes queue position from run-queue response","Card queue position comes from a newest-50 decisions page, so older queued waiters get no position.","correctness",True,"hard"),
62:("Fix checks value !== undefined not truthiness","A text-search chip with an empty value is dropped by a truthiness check before the user can type into it.","correctness",True,"medium"),
65:("Fix removes singleton shortcut so board lock is taken","Skipping the board lock for single-card groups inverts lock order against another path, causing deadlocks.","concurrency",True,"hard"),
66:("Fix reorders identity checks for null itemId","A stored ingress result with null itemId makes every later-linked card share the base identity, so its rules never run.","data-integrity",True,"hard"),
67:("Fix guards undefined externalId lookup","A renamed card without an external source looks up key undefined, matching every sourceless card and running rules on them.","correctness",False,"medium"),
68:("Fix gives companion a distinct identity when equal","Primary and companion pullRequestOpened evaluations get the same ingress id, so the companion is treated as a replay.","correctness",True,"hard"),
69:("Fix collects siblings for both relatedItem and companion","Merge evaluation only expands the issue's source, so cards linked to the PR's source never get evaluated.","correctness",True,"hard"),
73:("Fix initializes sharing flag to false","Repository caching is enabled before the change subscription is ready, so announcements can be missed and the cache goes stale.","concurrency",True,"medium"),
77:("Fix reports skipped count with a warning","Bulk move reports success and discards the skipped count even when cards were skipped.","correctness",True,"easy"),
78:("Fix tracks latest preview and dismisses stale toast","Concurrent reroute previews can finish out of order and show stale counts; an empty latest preview leaves the old warning.","concurrency",False,"medium"),
79:("Fix passes context to handler FGA check","Handler-level FGA check omits the request context that route-level check passes, so providers can wrongly deny.","security",True,"medium"),
82:("Fix pages the since-cursor query with limit","The since-cursor comment query has no row limit and can exceed the output buffer, failing every poll forever.","resource",True,"hard"),
83:("Fix keeps original tool name on converted tool","Renaming the web search tool leaves a named toolChoice pointing at a tool name absent from the tools list.","correctness",True,"medium"),
91:("Fix switches to full refund for cancelled waits","Cancelling during the queue wait keeps the reserved input cost although no provider call was made.","correctness",True,"medium"),
92:("Fix gates reads with feature flag during migration","During rolling deploy, writes via old instances desync the new column from the old one if the flag flips early.","data-integrity",True,"hard"),
}
D={0:"Fix is unrelated refactor; does not clearly address merged-reference instance context",
6:"Comment text truncated to scripts; defect not stated",7:"Fix only adds table entry; lookup not connected",
9:"Rule source metadata, not a code defect",10:"Rule source metadata, not a code defect",11:"Rule source metadata, not a code defect",
12:"Fix still ignores identifier-valued model options",13:"Changeset/process request",14:"Comment text truncated; defect not stated",
15:"Schema version metadata bump",16:"Fix does not change early return when no CSS imports",18:"Low-impact; unclear fix rejects .. segments",
20:"API compatibility nit; patch shows variant removed",24:"No fix hunk near commented lines",25:"Fix does not apply checks per group",
26:"Comment text truncated; defect not stated",27:"Comment text truncated; migration feature",28:"Migration mapping completeness, not code defect",
30:"Comment text truncated; defect not stated",34:"Test-only assertion nit",35:"Patch does not add abort-signal handling",
37:"Only a code comment changed",40:"Test timeout nit",42:"Test fixture typing",43:"Test style",44:"Test style",
47:"Fix does not unify issue reference",49:"Fix does not add routed-target support",50:"Trivial perf; fix unrelated",
52:"Test coverage request",53:"Rewrite; unclear lost-update addressed",54:"Redesign; fix ambiguous across files",
55:"Fix does not serialize intake write",56:"No fix hunk near commented lines",57:"No fix hunk near commented lines",
58:"No fix hunk near commented lines",61:"Queue check still after retry branch",63:"Test fixture style",64:"Test typing",
70:"Test coverage request",71:"Display detail nit",72:"Fix addresses different check, not precision",
74:"Fix does not separate publishing from subscription",75:"Prompt wording, not code",76:"Timeout tuning, not defect fix",
80:"Test-only",81:"No fix hunk near commented lines",84:"Test typing",85:"Type annotation style",86:"Type annotation style",
87:"Parity feature request",88:"Type annotation style",89:"Test style",90:"Process/QA documentation",
93:"File deleted; not a targeted fix",94:"Fix unrelated to constraint validation"}
L=[json.loads(l) for l in open(HERE/'all.jsonl')]
assert len(L)==95 and set(K)|set(D)==set(range(95)) and not set(K)&set(D)
with open(HERE/'triaged.jsonl','w') as f:
  for i,d in enumerate(L):
    if i in K:
      r,b,c,n,df=K[i]; o={"url":d['url'],"keep":True,"reason":r,"bug":b,"category":c,"needs_other_files":n,"difficulty":df}
    else:
      o={"url":d['url'],"keep":False,"reason":D[i]}
    f.write(json.dumps(o)+"\n")
kr=collections.Counter(L[i]['repo'] for i in K); print(len(K),95-len(K),kr)
print(collections.Counter(v[4] for v in K.values()),collections.Counter(v[3] for v in K.values()))
