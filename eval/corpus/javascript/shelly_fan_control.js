// Basement bathroom fan control -- runs ON the Shelly Plus 2PM, no HA required.
//
// Triggers on humidity RISE above a slow baseline rather than an absolute
// threshold: this basement baselines ~35% in winter and ~70% in summer, so any
// fixed number either over-runs the fan or never fires.
//
// Hardware notes learned the hard way on this device (Plus 2PM, fw 1.7.5):
//   - It is NOT a BTHome gateway. BTHome.GetStatus -> 404, no bthomesensor
//     components. Advertisements must be decoded here.
//   - BLE.Scanner methods are Capitalised (Subscribe/Start), not lowercase.
//   - active:true is required. Passive scanning returned 0/340 with service data.
//   - service_data values are RAW BYTE STRINGS, not hex.
//   - JSON.stringify on those raw bytes throws "Invalid UTF-8 string".
//   - KVS.Set on every advert throws "Too many calls in progress"; writes are
//     deduplicated and drained by a timer instead.
//   - KVS keys reject ':' so MACs are flattened.
//
// FIRST RUN: leave mac null. Discovered advertisers land in KVS as sd_<uuid>_<mac>.
// Set CFG.mac once a humidity-bearing payload is confirmed.

let CFG = {
  mac:      "aa:bb:cc:dd:ee:ff",   // BLU H&T ZB; null = discovery only, relay untouched
  fanId:    1,      // switch:1 = fan
  riseOn:   8.0,    // % over baseline to start
  fallOff:  3.0,    // % over baseline to stop
  emaTau:   4.0,    // baseline time constant, hours
  minRunS:  120,
  maxRunS:  2700,   // 45 min ceiling
  staleS:   3600,   // ignore readings older than this
};

let BTHOME_UUID = "fcd2";

let baseline = null;
let onSince  = null;   // null, never 0: a restart mid-run must not compute a 1.7Bs runtime
let lastHum  = null;
let lastSeen = 0;
let lastEma  = 0;

let pending = {};
let dirty   = [];
let demandUntil = 0;   // epoch seconds; external demand expires on its own
let stats   = { adverts: 0, svc: 0, bthome: 0 };

function now() { return Math.floor(Date.now() / 1000); }

function toHex(s) {
  let out = "";
  for (let i = 0; i < s.length && i < 20; i++) {
    let h = s.charCodeAt(i).toString(16);
    out += (h.length < 2 ? "0" : "") + h;
  }
  return out;
}

// BTHome v2 over raw bytes. Unknown objects are skipped by known length; a bare
// break would silently drop everything after the first unrecognised field.
function decode(sd) {
  let out = { t: null, h: null };
  if (!sd || sd.length < 2) return out;
  if (sd.charCodeAt(0) & 0x01) return out;      // encrypted -- do not guess
  let i = 1;
  while (i < sd.length) {
    let id = sd.charCodeAt(i);
    if (id === 0x00 || id === 0x01) { i += 2; }              // pid / battery
    else if (id === 0x02) { let v = sd.charCodeAt(i+1) | (sd.charCodeAt(i+2) << 8);
                            if (v & 0x8000) v -= 0x10000; out.t = v * 0.01; i += 3; }
    else if (id === 0x03) { out.h = (sd.charCodeAt(i+1) | (sd.charCodeAt(i+2) << 8)) * 0.01; i += 3; }
    else if (id === 0x05) { i += 4; }                        // illuminance u24
    else if (id === 0x21) { i += 2; }                        // motion
    else if (id === 0x2D) { i += 3; }                        // count u16
    else if (id === 0x2E) { out.h = sd.charCodeAt(i+1); i += 2; }
    else if (id === 0x3A) { i += 2; }                        // button
    else if (id === 0x45) { let v = sd.charCodeAt(i+1) | (sd.charCodeAt(i+2) << 8);
                            if (v & 0x8000) v -= 0x10000; out.t = v * 0.1; i += 3; }
    else break;
  }
  return out;
}

function queue(k, v) { if (pending[k] !== v) { pending[k] = v; dirty.push(k); } }

// Drain one write every 2s. Bounded no matter how fast adverts arrive.
Timer.set(2000, true, function () {
  if (dirty.length === 0) return;
  let k = dirty.splice(0, 1)[0];
  Shelly.call("KVS.Set", { key: k, value: pending[k] });
});

// External fan demand. Anything that can reach this device over the LAN can
// request the fan without owning the relay:
//   KVS.Set {key:"fan_demand", value:"<epoch seconds to hold until>"}
// An expiry rather than a boolean is deliberate: if the requester dies between
// on and off, a plain flag latches the fan on forever. Stale demand just lapses.
function readDemand() {
  Shelly.call("KVS.Get", { key: "fan_demand" }, function (res, err) {
    if (err || !res || typeof res.value === "undefined") return;
    let v = JSON.parse(res.value);
    if (typeof v === "number") demandUntil = v;
  });
}

function fanIsOn() {
  let st = Shelly.getComponentStatus("switch", CFG.fanId);
  return st ? !!st.output : false;
}
function setFan(on) { Shelly.call("Switch.Set", { id: CFG.fanId, on: on }); }

BLE.Scanner.Subscribe(function (ev, res) {
  if (ev !== BLE.Scanner.SCAN_RESULT) return;
  if (!res) return;
  stats.adverts++;
  if (!res.service_data) return;
  stats.svc++;

  let flat = res.addr.split(":").join("");
  for (let k in res.service_data) {
    queue("sd_" + k + "_" + flat, toHex(res.service_data[k]));
  }
  queue("ble_stats", JSON.stringify(stats));

  let sd = res.service_data[BTHOME_UUID];
  if (!sd) return;
  stats.bthome++;

  let d = decode(sd);
  if (d.h === null) return;

  if (CFG.mac === null) { queue("hum_" + flat, JSON.stringify({ h: d.h, t: d.t, rssi: res.rssi })); return; }
  if (res.addr !== CFG.mac) return;
  lastHum = d.h;
  lastSeen = now();
});

function tick() {
  readDemand();
  if (CFG.mac === null || lastHum === null) return;
  if (now() - lastSeen > CFG.staleS) return;

  let hum = lastHum;
  if (baseline === null) baseline = hum;
  let running = fanIsOn();
  let t = now();

  // Freeze the baseline while running or it chases the shower up and cuts off
  // early. Use real elapsed time; ticks are not guaranteed evenly spaced.
  if (!running) {
    let dt = lastEma === 0 ? 30 : (t - lastEma);
    if (dt > 0) { let a = dt / (CFG.emaTau * 3600 + dt); baseline = baseline + a * (hum - baseline); }
  }
  lastEma = t;

  let delta = hum - baseline;
  let ext   = t < demandUntil;

  if (!running) {
    onSince = null;
    if (delta >= CFG.riseOn || ext) {
      setFan(true); onSince = t;
      print("fan ON hum=", hum, " base=", baseline, " ext=", ext);
    }
    return;
  }

  // The fan is on but this script did not start it: someone used the wall
  // switch, or the script restarted mid-run. Leave it alone. Adopting it meant
  // cutting a deliberate manual run after minRunS -- observed live, the fan was
  // switched off ~2 minutes after going live.
  if (onSince === null) return;

  let ranFor = t - onSince;
  if (ranFor < CFG.minRunS) return;

  if (delta <= CFG.fallOff && !ext) {
    setFan(false); onSince = null;
    print("fan OFF hum=", hum, " ran=", ranFor);
  } else if (ranFor >= CFG.maxRunS) {
    // Ran the ceiling and humidity never fell: the baseline was wrong (ambient
    // shifted). Re-baseline, or the next tick sees the same delta and switches
    // straight back on -- a ~99% duty cycle that looks like it never turns off.
    setFan(false); onSince = null; baseline = hum;
    print("fan OFF (max runtime) re-baselined to ", hum);
  }
}

BLE.Scanner.Start({ duration_ms: BLE.Scanner.INFINITE_SCAN, active: true });
Timer.set(30000, true, tick);
