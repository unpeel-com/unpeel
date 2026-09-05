//
//  TerminalInputAccessoryView.swift
//  libghostty-spm
//

#if canImport(UIKit) && !targetEnvironment(macCatalyst)
    import UIKit

    @MainActor
    final class TerminalInputAccessoryView: UIView {
        weak var terminalView: UITerminalView?

        var style: TerminalInputAccessoryStyle = .default {
            didSet { refreshContent() }
        }

        private let barHeight: CGFloat = 52
        private let buttonSize: CGFloat = 36

        private lazy var blurView = UIVisualEffectView(
            effect: makeBarEffect()
        )
        private let scrollView = UIScrollView()
        private let stackView = UIStackView()
        /// Fixed cluster at the trailing edge; the key row scrolls under it.
        private let trailingStackView = UIStackView()
        private var blurLeadingConstraint: NSLayoutConstraint?
        private var blurTrailingConstraint: NSLayoutConstraint?
        private var blurTopConstraint: NSLayoutConstraint?
        private var blurBottomConstraint: NSLayoutConstraint?
        private var keyButtons: [AccessoryButton] = []
        private var modifierButtons: [(TerminalStickyModifierState.Modifier, AccessoryButton)] = []

        init(terminalView: UITerminalView) {
            self.terminalView = terminalView
            super.init(
                frame: CGRect(
                    x: 0,
                    y: 0,
                    width: 0,
                    height: Self.preferredHeight(for: barHeight)
                )
            )
            autoresizingMask = .flexibleWidth
            setupViews()
            applyBarChrome()
            refreshContent()
            terminalView.stickyModifiers.onChange = { [weak self] in
                self?.refreshContent()
            }
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        override var intrinsicContentSize: CGSize {
            CGSize(width: UIView.noIntrinsicMetric, height: preferredHeight)
        }

        func refreshContent() {
            let hasMarkedText = terminalView?.inputHandler.hasMarkedText ?? false
            let ctrlActivation = terminalView?.stickyModifiers.ctrl ?? .inactive
            let altActivation = terminalView?.stickyModifiers.alt ?? .inactive
            let commandActivation = terminalView?.stickyModifiers.command ?? .inactive

            keyButtons.forEach { $0.applyRegularStyle(style) }

            for (modifier, button) in modifierButtons {
                let activation = switch modifier {
                case .ctrl: ctrlActivation
                case .alt: altActivation
                case .command: commandActivation
                }
                button.applyModifierStyle(activation, isDisabled: hasMarkedText, style: style)
            }
        }

        func rebuildContent() {
            for stack in [stackView, trailingStackView] {
                stack.arrangedSubviews.forEach { view in
                    stack.removeArrangedSubview(view)
                    view.removeFromSuperview()
                }
            }
            keyButtons.removeAll()
            modifierButtons.removeAll()

            let items = terminalView?.inputAccessoryItems ?? TerminalInputAccessoryItem.defaultItems
            addArrangedViews(items.map(makeView(for:)))
            let trailing = terminalView?.inputAccessoryTrailingItems ?? []
            trailing.map(makeView(for:)).forEach { trailingStackView.addArrangedSubview($0) }
            trailingStackView.isHidden = trailing.isEmpty
            refreshContent()
        }

        private func setupViews() {
            backgroundColor = .clear

            blurView.translatesAutoresizingMaskIntoConstraints = false
            blurView.clipsToBounds = true
            addSubview(blurView)

            let leading = blurView.leadingAnchor.constraint(equalTo: leadingAnchor)
            let trailing = blurView.trailingAnchor.constraint(equalTo: trailingAnchor)
            let top = blurView.topAnchor.constraint(equalTo: topAnchor)
            let bottom = blurView.bottomAnchor.constraint(equalTo: bottomAnchor)
            blurLeadingConstraint = leading
            blurTrailingConstraint = trailing
            blurTopConstraint = top
            blurBottomConstraint = bottom

            NSLayoutConstraint.activate([
                leading,
                trailing,
                top,
                bottom,
                blurView.heightAnchor.constraint(equalToConstant: barHeight),
            ])

            scrollView.translatesAutoresizingMaskIntoConstraints = false
            scrollView.showsHorizontalScrollIndicator = false
            scrollView.showsVerticalScrollIndicator = false
            scrollView.alwaysBounceHorizontal = true
            scrollView.clipsToBounds = true
            blurView.contentView.addSubview(scrollView)

            trailingStackView.translatesAutoresizingMaskIntoConstraints = false
            trailingStackView.axis = .horizontal
            trailingStackView.alignment = .center
            trailingStackView.spacing = 8
            blurView.contentView.addSubview(trailingStackView)

            NSLayoutConstraint.activate([
                scrollView.leadingAnchor.constraint(equalTo: blurView.contentView.leadingAnchor),
                scrollView.trailingAnchor.constraint(
                    equalTo: trailingStackView.leadingAnchor,
                    constant: -4
                ),
                scrollView.topAnchor.constraint(equalTo: blurView.contentView.topAnchor),
                scrollView.bottomAnchor.constraint(equalTo: blurView.contentView.bottomAnchor),
                trailingStackView.trailingAnchor.constraint(
                    equalTo: blurView.contentView.trailingAnchor,
                    constant: -10
                ),
                trailingStackView.centerYAnchor.constraint(
                    equalTo: blurView.contentView.centerYAnchor
                ),
            ])

            stackView.translatesAutoresizingMaskIntoConstraints = false
            stackView.axis = .horizontal
            stackView.alignment = .center
            stackView.spacing = 8
            scrollView.addSubview(stackView)

            NSLayoutConstraint.activate([
                stackView.leadingAnchor.constraint(equalTo: scrollView.contentLayoutGuide.leadingAnchor, constant: 10),
                stackView.trailingAnchor.constraint(equalTo: scrollView.contentLayoutGuide.trailingAnchor, constant: -10),
                stackView.topAnchor.constraint(equalTo: scrollView.contentLayoutGuide.topAnchor),
                stackView.bottomAnchor.constraint(equalTo: scrollView.contentLayoutGuide.bottomAnchor),
                stackView.heightAnchor.constraint(equalTo: scrollView.frameLayoutGuide.heightAnchor),
            ])

            rebuildContent()
        }

        private func addArrangedViews(_ views: [UIView]) {
            views.forEach { stackView.addArrangedSubview($0) }
        }

        private func makeDivider() -> UIView {
            let view = UIView()
            view.translatesAutoresizingMaskIntoConstraints = false
            view.backgroundColor = .secondaryLabel.withAlphaComponent(0.28)
            view.layer.cornerRadius = 3
            NSLayoutConstraint.activate([
                view.widthAnchor.constraint(equalToConstant: 6),
                view.heightAnchor.constraint(equalToConstant: 6),
            ])
            return view
        }

        private func makeView(for item: TerminalInputAccessoryItem) -> UIView {
            switch item {
            case .esc:
                makeTrackedKeyButton(title: "Escape", systemImage: "escape", key: .esc)

            case .ctrl:
                makeTrackedModifierButton(title: "Control", systemImage: "control", modifier: .ctrl)

            case .alt:
                makeTrackedModifierButton(title: "Option", systemImage: "option", modifier: .alt)

            case .command:
                makeTrackedModifierButton(title: "Command", systemImage: "command", modifier: .command)

            case .tab:
                makeTrackedKeyButton(title: "Tab", systemImage: "arrow.right.to.line", key: .tab)

            case .arrowLeft:
                makeTrackedKeyButton(title: "Left", systemImage: "arrowtriangle.left.fill", key: .arrowLeft)

            case .arrowUp:
                makeTrackedKeyButton(title: "Up", systemImage: "arrowtriangle.up.fill", key: .arrowUp)

            case .arrowDown:
                makeTrackedKeyButton(title: "Down", systemImage: "arrowtriangle.down.fill", key: .arrowDown)

            case .arrowRight:
                makeTrackedKeyButton(title: "Right", systemImage: "arrowtriangle.right.fill", key: .arrowRight)

            case let .symbol(symbol):
                makeTrackedKeyButton(title: symbol, key: .symbol(symbol))

            case .paste:
                makeTrackedKeyButton(title: "Paste", systemImage: "doc.on.clipboard", key: .paste)

            case .backspace:
                makeBackspaceButton()

            case .hideKeyboard:
                makeDismissButton()

            case let .hostAction(id, systemImage, label):
                makeHostActionButton(id: id, systemImage: systemImage, label: label)

            case .divider:
                makeDivider()
            }
        }

        private func makeHostActionButton(
            id: String,
            systemImage: String,
            label: String
        ) -> AccessoryButton {
            let button = AccessoryButton(size: buttonSize) { [weak terminalView] in
                terminalView?.onAccessoryHostAction?(id)
            }
            button.accessibilityLabel = label
            button.setImage(UIImage(systemName: systemImage), for: .normal)
            keyButtons.append(button)
            return button
        }

        private func makeBackspaceButton() -> AccessoryButton {
            let button = AccessoryButton(size: buttonSize, repeatsOnHold: true) {
                [weak terminalView] in
                terminalView?.handleInputBarKey(.backspace)
            }
            button.accessibilityLabel = "Delete"
            button.setImage(UIImage(systemName: "delete.left"), for: .normal)
            keyButtons.append(button)
            return button
        }

        private func makeDismissButton() -> AccessoryButton {
            let button = AccessoryButton(size: buttonSize) { [weak terminalView] in
                _ = terminalView?.resignFirstResponder()
            }
            button.accessibilityLabel = "Hide Keyboard"
            button.setImage(
                UIImage(systemName: "keyboard.chevron.compact.down"),
                for: .normal
            )
            keyButtons.append(button)
            return button
        }

        private func makeTrackedModifierButton(
            title: String,
            systemImage: String,
            modifier: TerminalStickyModifierState.Modifier
        ) -> AccessoryButton {
            let button = makeModifierButton(
                title: title,
                systemImage: systemImage,
                modifier: modifier
            )
            modifierButtons.append((modifier, button))
            return button
        }

        private func makeTrackedKeyButton(
            title: String,
            systemImage: String? = nil,
            key: TerminalInputBarKey
        ) -> AccessoryButton {
            let button = makeKeyButton(title: title, systemImage: systemImage, key: key)
            keyButtons.append(button)
            return button
        }

        private func makeModifierButton(
            title: String,
            systemImage: String,
            modifier: TerminalStickyModifierState.Modifier
        ) -> AccessoryButton {
            let button = AccessoryButton(size: buttonSize) { [weak terminalView] in
                terminalView?.stickyModifiers.toggle(modifier)
            }
            button.accessibilityLabel = title
            button.setImage(UIImage(systemName: systemImage), for: .normal)
            return button
        }

        private func makeKeyButton(
            title: String,
            systemImage: String? = nil,
            key: TerminalInputBarKey
        ) -> AccessoryButton {
            let button = AccessoryButton(size: buttonSize) { [weak terminalView] in
                terminalView?.handleInputBarKey(key)
            }
            button.accessibilityLabel = title

            if let systemImage {
                button.setImage(UIImage(systemName: systemImage), for: .normal)
            } else {
                var configuration = UIButton.Configuration.plain()
                configuration.baseForegroundColor = .label
                configuration.title = title
                configuration.contentInsets = .zero
                configuration.attributedTitle = AttributedString(
                    title,
                    attributes: AttributeContainer([
                        .font: UIFont.monospacedSystemFont(ofSize: 13, weight: .semibold),
                    ])
                )
                button.configuration = configuration
            }

            return button
        }

        private var preferredHeight: CGFloat {
            Self.preferredHeight(for: barHeight)
        }

        private var currentOuterPadding: UIEdgeInsets {
            if #available(iOS 26, *) {
                UIEdgeInsets(top: 0, left: 8, bottom: 8, right: 8)
            } else {
                .zero
            }
        }

        private func makeBarEffect() -> UIVisualEffect {
            if #available(iOS 26, *) {
                let effect = UIGlassEffect(style: .regular)
                effect.isInteractive = true
                return effect
            } else {
                return UIBlurEffect(style: .systemUltraThinMaterial)
            }
        }

        private func applyBarChrome() {
            let padding = currentOuterPadding
            blurLeadingConstraint?.constant = padding.left
            blurTrailingConstraint?.constant = -padding.right
            blurTopConstraint?.constant = padding.top
            blurBottomConstraint?.isActive = !isFloatingBarLayout

            blurView.effect = makeBarEffect()
            blurView.layer.cornerCurve = .continuous
            blurView.layer.cornerRadius = if #available(iOS 26, *) {
                barHeight / 2
            } else {
                0
            }

            invalidateIntrinsicContentSize()
        }

        private var isFloatingBarLayout: Bool {
            if #available(iOS 26, *) {
                true
            } else {
                false
            }
        }

        private static func preferredHeight(for barHeight: CGFloat) -> CGFloat {
            if #available(iOS 26, *) {
                barHeight + 8
            } else {
                barHeight
            }
        }
    }

    private final class AccessoryButton: UIButton {
        private let size: CGFloat
        private let handler: () -> Void
        private let lockIndicator = UIView()

        /// Press-and-hold key repeat (backspace). We drive it ourselves because
        /// iOS only removes the keyboard's dictation mic via secure text entry,
        /// which also kills the system backspace's native auto-repeat. Timings
        /// mirror the platform: a hold delay, then a fast repeat cadence.
        private let repeatsOnHold: Bool
        private var repeatTimer: Timer?
        private let repeatInitialDelay: TimeInterval = 0.4
        private let repeatInterval: TimeInterval = 0.08

        init(size: CGFloat, repeatsOnHold: Bool = false, handler: @escaping () -> Void) {
            self.size = size
            self.repeatsOnHold = repeatsOnHold
            self.handler = handler
            super.init(frame: .zero)

            translatesAutoresizingMaskIntoConstraints = false
            layer.cornerRadius = size / 2
            layer.cornerCurve = .continuous
            clipsToBounds = true

            tintColor = .label
            backgroundColor = UIColor.systemGray5.withAlphaComponent(0.92)

            titleLabel?.font = .systemFont(ofSize: 14, weight: .medium)
            imageView?.contentMode = .scaleAspectFit

            NSLayoutConstraint.activate([
                widthAnchor.constraint(equalToConstant: size),
                heightAnchor.constraint(equalToConstant: size),
            ])

            lockIndicator.translatesAutoresizingMaskIntoConstraints = false
            lockIndicator.backgroundColor = tintColor
            lockIndicator.layer.cornerRadius = 1.5
            lockIndicator.isHidden = true
            addSubview(lockIndicator)

            NSLayoutConstraint.activate([
                lockIndicator.centerXAnchor.constraint(equalTo: centerXAnchor),
                lockIndicator.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -3),
                lockIndicator.widthAnchor.constraint(equalToConstant: 14),
                lockIndicator.heightAnchor.constraint(equalToConstant: 3),
            ])

            if repeatsOnHold {
                // Fire immediately on press, then auto-repeat after a hold
                // delay; any touch release/cancel stops it.
                addTarget(self, action: #selector(repeatTouchDown), for: .touchDown)
                addTarget(
                    self,
                    action: #selector(stopRepeating),
                    for: [.touchUpInside, .touchUpOutside, .touchCancel, .touchDragExit]
                )
            } else {
                addAction(UIAction { [weak self] _ in
                    self?.handler()
                }, for: .touchUpInside)
            }
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        @objc private func repeatTouchDown() {
            handler()
            repeatTimer?.invalidate()
            repeatTimer = Timer.scheduledTimer(
                timeInterval: repeatInitialDelay,
                target: self,
                selector: #selector(startContinuousRepeat),
                userInfo: nil,
                repeats: false
            )
        }

        @objc private func startContinuousRepeat() {
            repeatTimer?.invalidate()
            repeatTimer = Timer.scheduledTimer(
                timeInterval: repeatInterval,
                target: self,
                selector: #selector(fireRepeat),
                userInfo: nil,
                repeats: true
            )
        }

        @objc private func fireRepeat() {
            handler()
        }

        @objc private func stopRepeating() {
            repeatTimer?.invalidate()
            repeatTimer = nil
        }

        // Stop (and break the timer's strong ref) if the bar is rebuilt or
        // removed mid-hold.
        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil { stopRepeating() }
        }

        func applyRegularStyle(_ style: TerminalInputAccessoryStyle) {
            isEnabled = true
            alpha = 1
            tintColor = style.regularForeground
            backgroundColor = style.regularBackground
            lockIndicator.isHidden = true
            lockIndicator.backgroundColor = tintColor
            configuration?.baseForegroundColor = tintColor
        }

        func applyModifierStyle(
            _ activation: TerminalStickyModifierState.Activation,
            isDisabled: Bool,
            style: TerminalInputAccessoryStyle
        ) {
            isEnabled = !isDisabled
            alpha = isDisabled && activation == .inactive ? 0.45 : 1

            let isActive = activation != .inactive
            tintColor = isActive ? style.activeForeground : style.regularForeground
            backgroundColor = isActive ? style.activeBackground : style.regularBackground
            lockIndicator.isHidden = activation != .locked
            lockIndicator.backgroundColor = tintColor
            configuration?.baseForegroundColor = tintColor
        }
    }
#endif
