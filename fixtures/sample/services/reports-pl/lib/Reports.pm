package Reports;

use strict;
use warnings;
use Reports::Formatter;

sub new {
    my ($class, %args) = @_;
    return bless { rows => [] }, $class;
}

sub add_row {
    my ($self, $row) = @_;
    validate_row($row);
    push @{ $self->{rows} }, $row;
    return $self;
}

sub render {
    my ($self) = @_;
    my $body = format_rows($self->{rows});
    return $self->wrap($body);
}

sub wrap {
    my ($self, $body) = @_;
    return "<report>$body</report>";
}

sub validate_row {
    my ($row) = @_;
    return defined $row;
}

sub format_rows {
    my ($rows) = @_;
    return join ',', @{$rows};
}

1;
