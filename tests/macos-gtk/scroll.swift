// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
// Native input probe: swift tests/macos-gtk/scroll.swift PID SCREEN_X SCREEN_Y DELTA pixel|line
// Screen coordinates use CoreGraphics' top-left origin. Run against isolated demo data.
import AppKit

precondition(CommandLine.arguments.count == 6,
             "usage: scroll.swift PID SCREEN_X SCREEN_Y DELTA pixel|line")
let pid = pid_t(CommandLine.arguments[1])!
let point = CGPoint(x: Double(CommandLine.arguments[2])!,
                    y: Double(CommandLine.arguments[3])!)
let delta = Int32(CommandLine.arguments[4])!
let unit: CGScrollEventUnit = CommandLine.arguments[5] == "pixel" ? .pixel : .line
NSRunningApplication(processIdentifier: pid)?.activate(options: [])
CGWarpMouseCursorPosition(point)
let motion = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved,
                     mouseCursorPosition: point, mouseButton: .left)!
motion.post(tap: .cghidEventTap)
Thread.sleep(forTimeInterval: 0.2)
for index in 0..<6 {
    let event = CGEvent(scrollWheelEvent2Source: nil, units: unit, wheelCount: 2,
                        wheel1: delta, wheel2: 0, wheel3: 0)!
    event.location = point
    if unit == .pixel {
        event.setIntegerValueField(.scrollWheelEventScrollPhase, value: index == 0 ? 1 : 2)
    }
    // Post through the system event stream: postToPid produces windowless events,
    // which do not exercise GDK's normal surface hit testing.
    event.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.1)
}
if unit == .pixel {
    let stop = CGEvent(scrollWheelEvent2Source: nil, units: unit, wheelCount: 2,
                       wheel1: 0, wheel2: 0, wheel3: 0)!
    stop.location = point
    stop.setIntegerValueField(.scrollWheelEventScrollPhase, value: 4)
    stop.post(tap: .cghidEventTap)
}
