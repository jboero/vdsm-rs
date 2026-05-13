# Trivial wrapper around the selinux-policy-devel Makefile so
# `make -C packaging/selinux` builds vdsm-rs.pp from .te + .fc.

# SELinux policy module names are C identifiers (underscores only),
# and the .te / .fc / .pp filenames must match the policy_module()
# call. Hence vdsm_rs, not vdsm-rs.
TARGETS  := vdsm_rs
DEVEL_MK := /usr/share/selinux/devel/Makefile

all: $(addsuffix .pp,$(TARGETS))

$(addsuffix .pp,$(TARGETS)): %.pp: %.te %.fc
	$(MAKE) -f $(DEVEL_MK) $@

clean:
	$(MAKE) -f $(DEVEL_MK) clean
	rm -f $(addsuffix .pp,$(TARGETS)) $(addsuffix .pp.bz2,$(TARGETS))

.PHONY: all clean
