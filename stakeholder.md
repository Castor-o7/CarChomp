# This is a the directive from the stakeholder: D. Kevin McGrath

Capstone Project: Raspberry Pi GPS Tracking and Geocaching Carputer

Motivation

I really enjoy driving. I also really enjoy technology. Being able to combine them in a meaningful way is one of the joys of being a computer scientist. Have an itch? Scratch it!

Seeing new places and driving on new roads is great fun, but I don’t always know if the road I’m on is a new road (to me) or just one I haven’t been on in a long time. While this might seem like an odd thing to worry about, we all have our quirks, and this is one of mine. Apple Maps, Google Maps, and stand-alone GPS units all have the routing and “where am I?” problem solved. But none that I have found to date keep track of where I’ve been historically.

Having the ability to glance at the Pi display and see where I am and if I’ve been here before would be awesome. As a stretch goal, having the ability to load topographical overlays and map off-road trails would be awesome.

As a stretch goal, offering routing based on a variety of factors, including:

Scenic routes
Avoiding highways
Towing a trailer (length, weight, height considerations)
Optimizing for fuel efficiency
Optimizing for electric vehicle charging stations
Why use a Pi?

Because I have one, they are ubiquitous, they are cheap, and it is very easy to install one on a mount for a car. Plus, there are extended-temperature-range options available, which is a good idea for a car.

Objective

There are a variety of commercial solutions for GPS tracking, but none that I have found that meet all of my needs.

What I’m looking for is a Raspberry Pi-based car computer. Some specific features I’m looking for:

Tracks my drives as GPS tracks and alerts me when I’m on a new road
Automatic launch of app at boot time
A touch-based interface
A postgresql database storing all of the geographic information
Integration with an RTL-SDR or other radio system to receive surrounding APRS broadcasts
Locations of APRS users should be logged in the database
Locations should optionally be displayed on the map as an overlay layer.
No radio license required for reception.
A display overlaying GPS tracks on something like openstreetmaps data. Basically a map display showing current location and any relevant GPS tracks.
In this context, relevant GPS tracks include any track within a specified distance of the current location, or any track that uses the current road.
Overlaying multiple tracks on the map might get very busy, so having some way to disable certain tracks would be useful.
Constant storage of GPS track data, using something like the APRS smart beaconing algorithm.
A web-based interface to view GPS tracks and manage the system, including:
A data import function to ingest GPS tracks in various formats
A data export function to export GPS tracks in various formats
System updates
A dashboard displaying health of the system, current patch level, number of GPS tracks, etc.
The ability to run headless, with all functionality accessible via the web interface
Online and offline map functionality, with the ability to download map data for offline use
Automatic wifi hotspot functionality, with the ability to connect to known networks and create a hotspot when no known networks are available
Hardware

For hardware, a Raspberry Pi 4, a touch screen, and a GPS module would be provided. As this would be a PoC, a Pi 4 with 4GB of RAM coupled with a Raspberry Pi Touch Display or external touch display would be sufficient. Ultimately, something like a reTerminal or other integrated HMI-type solution will be used (bare electronics in a car is not a great idea!). I don’t have those to just loan out, though.

Deliverables

There are several deliverables for this project:

A working PoC on the provided hardware
An SD card image with all software and configuration ready to go at first boot
A script or github action to keep an SD card image up-to-date with upstream changes
A detailed write-up of the process to create such a device, including any scripts or code used
An installation script which could be used on an existing Pi to install necessary software and configurations