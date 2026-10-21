# usage: calib_collect.py <out.jsonl> <cases.jsonl>...: one row per returned result, with its
# features and whether it is correct (within the case tolerance of the expected point).
import json, os, sys, math, urllib.parse, http.client
out = open(sys.argv[1], "w")
host, port = os.environ.get("GEKO_URL", "127.0.0.1:8080").rsplit(":", 1)
conn = http.client.HTTPConnection(host, int(port))
def dist(a, b, c, d):
    r = 6371000.0
    p1, p2 = math.radians(a), math.radians(c)
    dp, dl = p2 - p1, math.radians(d - b)
    h = math.sin(dp/2)**2 + math.cos(p1)*math.cos(p2)*math.sin(dl/2)**2
    return 2*r*math.asin(math.sqrt(h))
for path in sys.argv[2:]:
    for n, line in enumerate(open(path)):
        if not line.strip() or line.startswith("//"):
            continue
        c = json.loads(line)
        if "keystroke" in path or c["category"].startswith("keystroke"):
            if n % 4:
                continue
        route = "autocomplete" if c.get("autocomplete") else "search"
        params = {"text": c["query"], "debug": "true", "size": "5"}
        if c.get("focus_lat") is not None:
            params["focus.point.lat"], params["focus.point.lon"] = c["focus_lat"], c["focus_lon"]
        conn.request("GET", f"/v1/{route}?" + urllib.parse.urlencode(params))
        body = json.loads(conn.getresponse().read())
        feats = body.get("features", [])
        scores = [f["properties"]["score"] for f in feats]
        for i, f in enumerate(feats):
            lon, lat = f["geometry"]["coordinates"]
            ok = dist(lat, lon, c["lat"], c["lon"]) <= c["tolerance_m"] or f["properties"].get("gid") == c.get("gid")
            out.write(json.dumps({
                "set": path.rsplit("/", 1)[-1], "category": c["category"], "route": route, "rank": i,
                "score": scores[i], "top": scores[0], "next": scores[i+1] if i+1 < len(scores) else None,
                "prev": scores[i-1] if i else None, "n": len(scores),
                "match_type": f["properties"]["match_type"], "layer": f["properties"].get("layer"),
                "confidence": f["properties"]["confidence"], "ok": ok}) + "\n")
