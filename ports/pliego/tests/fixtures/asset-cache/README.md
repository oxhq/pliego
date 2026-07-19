# Content-addressed asset cache fixture

`first.js` and `renamed.js` are byte-identical but have different source
paths and URLs. The checker renders them twice, changes both byte streams and
their declared digest, then verifies a deliberately incorrect declaration is
rejected before Servo starts.
