# BareEye TODO

## 1. Hardware foundation

* [x] Enumerate Windows video capture devices
* [ ] Identify the Polycom EagleEye IV reliably
* [x] Open a live 1080p video stream
* [x] Enumerate supported camera-control properties
* [x] Read pan, tilt, zoom, and focus ranges
* [x] Read current pan, tilt, and zoom positions where supported
* [x] Implement absolute PTZ control
* [ ] Investigate relative / velocity PTZ control support
* [ ] Test whether the EagleEye accepts sub-degree PTZ positions despite its advertised 1° step
* [ ] Calibrate the EagleEye USB zoom-control value to its actual optical zoom / FOV curve
* [ ] Implement safe PTZ limit handling

## 2. Application foundation

* [x] Create native Windows application window
* [x] Display the live camera feed with low latency
* [ ] Add GPU-efficient frame presentation
* [x] Add camera status and PTZ diagnostics
* [ ] Add mouse-controlled manual pan, tilt, and zoom
* [x] Add keyboard controls for development and recovery
* [ ] Add selectable 5° manual PTZ stepping
* [ ] Add selectable 1° fine PTZ stepping for high-zoom control
* [ ] Keep capture, vision, control, and UI work isolated from each other

## 3. Vision pipeline

* [x] Integrate ONNX Runtime
* [x] Enable NVIDIA CUDA inference on Windows
* [x] Add person detection
* [x] Add face detection
* [x] Draw interactive detection boxes over the live image
* [ ] Maintain stable identities between frames
* [ ] Add short-term target re-identification after occlusion
* [ ] Keep the vision pipeline real-time by dropping stale frames

## 4. Target selection

* [x] Select a detected person or face by clicking its box
* [x] Deselect or switch targets cleanly
* [ ] Add manual drag-to-select tracking for arbitrary objects
* [ ] Store target appearance and motion state
* [x] Recover a selected target after brief loss
* [ ] Clearly display locked, searching, lost, and reacquired states

## 5. PTZ tracking controller

* [x] Convert image-space target error into pan and tilt commands
* [ ] Maintain sub-degree internal PTZ targets and accumulate fractional corrections
* [ ] Quantize physical PTZ commands to the camera's actual supported hardware step
* [x] Add smoothing and dead zones
* [ ] Add target-motion prediction
* [ ] Add zoom-dependent control sensitivity
* [ ] Add acceleration and movement-rate limits
* [x] Prevent oscillation and detection-jitter chasing
* [ ] Maintain stable tracking during simultaneous pan, tilt, and zoom

## 6. Framing and zoom lock

* [x] Add center-follow mode
* [ ] Add loose follow mode
* [ ] Add exact framing-lock mode
* [ ] Preserve target position within the frame
* [ ] Preserve target apparent size using optical zoom
* [ ] Add human-aware framing using face or body landmarks
* [ ] Restore previous framing after temporary target loss
* [ ] Handle minimum and maximum optical zoom gracefully

## 7. Pan-limit wraparound

* [ ] Model the camera's usable pan range and rear blind sector
* [ ] Detect when a tracked target is approaching a mechanical pan limit
* [ ] Predict whether the target is continuing through the blind sector
* [ ] Automatically widen optical zoom before intentional target loss
* [ ] Preserve target trajectory and appearance during wraparound
* [ ] Perform controlled long-way pan to the opposite limit
* [ ] Predict the target's expected re-entry region
* [ ] Reacquire the same target on the opposite side
* [ ] Restore tracking and previous framing after reacquisition
* [ ] Abort wraparound safely when confidence becomes too low

## 8. Tracking robustness

* [ ] Handle temporary occlusion
* [ ] Handle multiple nearby people
* [ ] Reduce identity switching
* [ ] Handle people entering and leaving the frame
* [ ] Handle rapid target motion
* [ ] Handle tracking at maximum optical zoom
* [ ] Recover gracefully when the camera cannot find the target
* [ ] Provide an immediate manual override

## 9. Configuration and usability

* [ ] Save selected camera and device settings
* [ ] Save control tuning parameters
* [ ] Save preferred tracking and framing modes
* [ ] Add configurable PTZ speed and tracking aggressiveness
* [ ] Add configurable detection confidence thresholds
* [ ] Add diagnostics for inference speed, frame rate, and tracking latency
* [ ] Provide sensible defaults without requiring configuration

## 10. Validation

* [ ] Add unit tests for tracking-state transitions
* [ ] Add unit tests for framing calculations
* [ ] Add unit tests for PTZ limit logic
* [ ] Add unit tests for wraparound prediction
* [ ] Test prolonged person tracking
* [ ] Test manual arbitrary-object tracking
* [ ] Test full optical zoom tracking
* [ ] Test repeated pan-limit crossings
* [ ] Test loss and reacquisition with multiple people present
* [ ] Verify clean shutdown and camera release
