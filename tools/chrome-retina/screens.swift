import AppKit
let main = NSScreen.screens[0].frame
for s in NSScreen.screens {
  let f = s.frame
  // convert to top-left-origin global coords (what Chrome's --window-position uses)
  let topY = main.height - (f.origin.y + f.height)
  print("\(s.localizedName) scale=\(s.backingScaleFactor) x=\(Int(f.origin.x)) y=\(Int(topY)) w=\(Int(f.width)) h=\(Int(f.height)) builtin=\(s.localizedName.contains("Built-in"))")
}
